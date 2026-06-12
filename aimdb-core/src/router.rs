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

use alloc::{string::String, sync::Arc, vec::Vec};

use crate::connector::IngestFn;

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
    routes: Vec<Route>,
}

impl Router {
    /// Create a new router with the given routes
    pub fn new(routes: Vec<Route>) -> Self {
        Self { routes }
    }

    /// Route a message to the appropriate record(s)
    ///
    /// Synchronous: the ingest callback deserializes and produces in place
    /// (`Producer::produce` is sync and infallible, design 029) — nothing on
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
        for route in &self.routes {
            if route.resource_id.as_ref() == resource_id {
                matched = true;
                match (route.ingest)(ctx, payload) {
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
        let mut ids: Vec<Arc<str>> = self.routes.iter().map(|r| r.resource_id.clone()).collect();

        // Deduplicate by converting to strings for comparison
        ids.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        ids.dedup_by(|a, b| a.as_ref() == b.as_ref());

        ids
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
        crate::RuntimeContext::new(Arc::new(aimdb_executor::test_support::NoopRuntimeOps))
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
}
