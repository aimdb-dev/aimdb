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

use crate::connector::{IngestFn, MatchIngestFn};
use crate::inbound_key::{KeyId, KeyTable};
use crate::topic_pattern::{
    ExactGrammar, Spans, TopicFilter, TopicGrammar, TopicMatch, MAX_CAPTURES,
};

/// A single routing entry
///
/// Maps one (resource_id, type) pair to a fused ingest callback.
/// Multiple routes can exist for the same resource_id (different types).
///
/// # Resource ID Examples
///
/// - MQTT: "sensors/temperature" (topic)
/// - Kafka: "events:0" (topic:partition)
/// - HTTP: "/api/v1/sensors" (path)
/// - DDS: "TelemetryData" (topic name)
/// - Shmem: "temperature_buffer" (segment name)
pub struct Route {
    /// Resource identifier to match (reference-counted for proper memory management)
    ///
    /// Examples: MQTT topic, Kafka topic, HTTP path, DDS topic, shmem segment
    ///
    /// Uses `Arc<str>` instead of `&'static str` to avoid memory leaks from `Box::leak()`.
    /// This adds ~8 bytes overhead per route (Arc control block) but enables proper cleanup.
    pub resource_id: Arc<str>,

    /// Fused ingest callback: deserialize + produce in one typed closure
    /// built at registration time (no `Box<dyn Any>` per message).
    pub ingest: IngestFn,
}

/// Generic message router for connector dispatch
///
/// Routes incoming messages to the matching records' ingest callbacks based on
/// resource_id. Uses linear search which is efficient for <100 routes.
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
    ingest: MatchIngestFn,
}

impl CompiledRoute {
    /// A route comparing `resource_id` as a string.
    pub(crate) fn exact(resource_id: Arc<str>, ingest: IngestFn) -> Self {
        Self {
            filter: resource_id,
            matcher: None,
            names: Box::new([]),
            key: None,
            ingest: crate::connector::ignore_match(ingest),
        }
    }

    /// A route matching through `filter`.
    pub(crate) fn pattern(
        filter: Box<dyn TopicFilter>,
        names: Box<[Box<str>]>,
        key: Option<(Arc<KeyTable>, usize)>,
        ingest: MatchIngestFn,
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
    /// Create a new router with the given routes
    pub fn new(routes: Vec<Route>) -> Self {
        Self {
            routes: routes
                .into_iter()
                .map(|r| CompiledRoute::exact(r.resource_id, r.ingest))
                .collect(),
            grammar: &ExactGrammar,
        }
    }

    pub(crate) fn compiled(grammar: &'static dyn TopicGrammar, routes: Vec<CompiledRoute>) -> Self {
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

/// Builder for constructing routers
///
/// Provides a fluent API for adding routes before creating the router.
pub struct RouterBuilder {
    routes: Vec<Route>,
}

impl RouterBuilder {
    /// Create a new router builder
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    /// Create a router builder from a collection of routes
    ///
    /// This is a convenience method for automatic router construction from
    /// `AimDb::collect_inbound_routes()`. The resource_ids are converted to
    /// `Arc<str>` for proper memory management.
    ///
    /// # Arguments
    /// * `routes` - Vector of (resource_id, ingest) tuples
    pub fn from_routes(routes: Vec<(String, IngestFn)>) -> Self {
        let mut builder = Self::new();
        for (resource_id, ingest) in routes {
            // Convert String to Arc<str> - no leaking needed!
            let resource_id_arc: Arc<str> = Arc::from(resource_id.as_str());
            builder = builder.add_route(resource_id_arc, ingest);
        }
        builder
    }

    /// Add a route to the router
    ///
    /// # Arguments
    /// * `resource_id` - Resource identifier to match (as `Arc<str>`)
    /// * `ingest` - Fused ingest callback (deserialize + produce)
    ///
    /// # Resource ID Memory Management
    /// The resource_id is stored as `Arc<str>` for proper reference counting and cleanup.
    /// You can create an `Arc<str>` from:
    /// - String literal: `Arc::from("sensors/temperature")`
    /// - Owned String: `Arc::from(string.as_str())`
    pub fn add_route(mut self, resource_id: Arc<str>, ingest: IngestFn) -> Self {
        self.routes.push(Route {
            resource_id,
            ingest,
        });
        self
    }

    /// Build the router
    ///
    /// Consumes the builder and returns a configured Router.
    pub fn build(self) -> Router {
        Router::new(self.routes)
    }

    /// Get the number of routes that will be created
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

impl Default for RouterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A `RuntimeContext` backed by the shared no-op RuntimeOps.
    fn test_ctx() -> crate::RuntimeContext {
        crate::RuntimeContext::new(Arc::new(crate::executor::test_support::NoopRuntimeOps))
    }

    /// Ingest callback that counts successful invocations.
    fn counting_ingest(call_count: Arc<AtomicUsize>) -> IngestFn {
        Arc::new(move |_ctx, _payload| {
            call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    #[test]
    fn test_single_route() {
        let call_count = Arc::new(AtomicUsize::new(0));

        let routes = vec![Route {
            resource_id: Arc::from("test/resource"),
            ingest: counting_ingest(call_count.clone()),
        }];

        let router = Router::new(routes);

        router
            .route("test/resource", b"dummy", &test_ctx())
            .unwrap();

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_multiple_routes_same_resource() {
        let call_count1 = Arc::new(AtomicUsize::new(0));
        let call_count2 = Arc::new(AtomicUsize::new(0));

        let routes = vec![
            Route {
                resource_id: Arc::from("shared/resource"),
                ingest: counting_ingest(call_count1.clone()),
            },
            Route {
                resource_id: Arc::from("shared/resource"),
                ingest: counting_ingest(call_count2.clone()),
            },
        ];

        let router = Router::new(routes);

        router
            .route("shared/resource", b"dummy", &test_ctx())
            .unwrap();

        // Both ingest callbacks should be called
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_unknown_resource() {
        let call_count = Arc::new(AtomicUsize::new(0));

        let routes = vec![Route {
            resource_id: Arc::from("test/resource"),
            ingest: counting_ingest(call_count.clone()),
        }];

        let router = Router::new(routes);

        // Should not panic on unknown resource
        router
            .route("unknown/resource", b"dummy", &test_ctx())
            .unwrap();

        assert_eq!(call_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_resource_ids_deduplication() {
        let routes = vec![
            Route {
                resource_id: Arc::from("resource1"),
                ingest: counting_ingest(Arc::new(AtomicUsize::new(0))),
            },
            Route {
                resource_id: Arc::from("resource1"), // Duplicate
                ingest: counting_ingest(Arc::new(AtomicUsize::new(0))),
            },
            Route {
                resource_id: Arc::from("resource2"),
                ingest: counting_ingest(Arc::new(AtomicUsize::new(0))),
            },
        ];

        let router = Router::new(routes);
        let ids = router.resource_ids();

        assert_eq!(ids.len(), 2);
        assert!(ids.iter().any(|id| id.as_ref() == "resource1"));
        assert!(ids.iter().any(|id| id.as_ref() == "resource2"));
    }

    #[test]
    fn test_ingest_receives_payload_and_ctx() {
        let seen_len = Arc::new(AtomicUsize::new(0));
        let seen_len_clone = seen_len.clone();

        let ingest: IngestFn = Arc::new(move |_ctx, payload| {
            seen_len_clone.store(payload.len(), Ordering::SeqCst);
            Ok(())
        });

        let routes = vec![Route {
            resource_id: Arc::from("ctx/resource"),
            ingest,
        }];

        let router = Router::new(routes);
        router.route("ctx/resource", b"dummy", &test_ctx()).unwrap();

        assert_eq!(seen_len.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn test_ingest_error_does_not_propagate() {
        let ingest: IngestFn = Arc::new(|_ctx, _payload| Err("deserialize failed".into()));

        let routes = vec![Route {
            resource_id: Arc::from("err/resource"),
            ingest,
        }];

        let router = Router::new(routes);

        // Ingest failures are logged, not propagated.
        router.route("err/resource", b"dummy", &test_ctx()).unwrap();
    }

    // ---- pattern routes ---------------------------------------------------

    use crate::topic_pattern::{test_support::Plus, TopicPattern};
    use core::num::NonZeroU16;
    use std::sync::Mutex;

    type Seen = Arc<Mutex<Vec<(String, Vec<Option<String>>, Option<usize>)>>>;

    /// Records the topic, the named captures and the key index.
    fn recording(seen: &Seen, names: &'static [&'static str]) -> MatchIngestFn {
        let seen = seen.clone();
        Arc::new(move |_ctx, m, _payload| {
            let caps = names.iter().map(|n| m.get(n).map(String::from)).collect();
            seen.lock()
                .unwrap()
                .push((m.topic().to_string(), caps, m.key().map(|k| k.index())));
            Ok(())
        })
    }

    fn pattern_route(
        topic: &str,
        ingest: MatchIngestFn,
        key: Option<(Arc<KeyTable>, usize)>,
    ) -> CompiledRoute {
        let pattern = TopicPattern::parse(topic).unwrap();
        let names = pattern.capture_names().map(Box::from).collect();
        CompiledRoute::pattern(Plus.compile(&pattern).unwrap(), names, key, ingest)
    }

    fn exact_route(topic: &str, ingest: IngestFn) -> CompiledRoute {
        CompiledRoute::exact(Arc::from(topic), ingest)
    }

    #[test]
    fn pattern_route_receives_topic_and_captures() {
        let seen: Seen = Default::default();
        let router = Router::compiled(
            &Plus,
            vec![pattern_route(
                "{site}/+/{dev}",
                recording(&seen, &["site", "dev", "nope"]),
                None,
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
    fn exact_and_pattern_routes_both_receive_a_message() {
        let exact = Arc::new(AtomicUsize::new(0));
        let seen: Seen = Default::default();
        let router = Router::compiled(
            &Plus,
            vec![
                exact_route("s/kitchen/t", counting_ingest(exact.clone())),
                pattern_route("s/{d}/t", recording(&seen, &["d"]), None),
                pattern_route("s/kitchen/t", recording(&seen, &[]), None),
            ],
        );
        router.route("s/kitchen/t", b"", &test_ctx()).unwrap();

        assert_eq!(exact.load(Ordering::SeqCst), 1);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        // A literal pattern route still receives the topic.
        assert_eq!(seen[1].0, "s/kitchen/t");
    }

    #[test]
    fn keyed_route_assigns_keys_and_drops_when_full() {
        let seen: Seen = Default::default();
        let table = Arc::new(KeyTable::new(NonZeroU16::new(2).unwrap()));
        let router = Router::compiled(
            &Plus,
            vec![pattern_route(
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
        let router = Router::compiled(
            &Plus,
            vec![pattern_route("{x}", recording(&seen, &[]), None)],
        );
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
        let noop = || counting_ingest(Arc::new(AtomicUsize::new(0)));
        let seen: Seen = Default::default();
        let router = Router::compiled(
            &Plus,
            vec![
                exact_route("s/kitchen/t", noop()),
                exact_route("other/x", noop()),
                exact_route("s/+/t", noop()),
                pattern_route("s/{d}/t", recording(&seen, &[]), None),
            ],
        );
        assert_eq!(subscriptions(&router), ["other/x", "s/+/t"]);
    }

    #[test]
    fn plain_router_subscribes_each_id_once() {
        let noop = || counting_ingest(Arc::new(AtomicUsize::new(0)));
        let router = Router::new(vec![
            Route {
                resource_id: Arc::from("a"),
                ingest: noop(),
            },
            Route {
                resource_id: Arc::from("a"),
                ingest: noop(),
            },
            Route {
                resource_id: Arc::from("b"),
                ingest: noop(),
            },
        ]);
        assert_eq!(subscriptions(&router), ["a", "b"]);
    }
}
