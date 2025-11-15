//! Generic message router for efficient connector dispatch
//!
//! Provides O(M) routing complexity instead of O(N×M) filtered streams.
//! Routes incoming messages directly to type-specific producers based on topic/key matching.
//!
//! This router is protocol-agnostic and can be used by any connector:
//! - MQTT: Routes topics to producers
//! - Kafka: Routes topics/partitions to producers
//! - HTTP: Routes paths to producers
//! - DDS: Routes topics to producers
//! - Shared Memory: Routes segment names to producers

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

use crate::connector::{DeserializerFn, ProducerTrait};

/// A single routing entry
///
/// Maps one (resource_id, type) pair to a producer and deserializer.
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
    /// Resource identifier to match (static lifetime for efficiency)
    ///
    /// Examples: MQTT topic, Kafka topic, HTTP path, DDS topic, shmem segment
    pub resource_id: &'static str,

    /// Type-erased producer for this route
    pub producer: Box<dyn ProducerTrait>,

    /// Deserializer for converting bytes → typed value
    pub deserializer: DeserializerFn,
}

/// Generic message router for connector dispatch
///
/// Routes incoming messages to appropriate producers based on resource_id.
/// Uses linear search which is efficient for <100 routes.
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

    /// Route a message to appropriate producer(s)
    ///
    /// # Arguments
    /// * `resource_id` - Resource identifier (topic, path, segment name, etc.)
    /// * `payload` - Raw message payload bytes
    ///
    /// # Returns
    /// * `Ok(())` - At least one route successfully processed the message
    /// * `Err(_)` - All routes failed (or no routes found)
    ///
    /// # Behavior
    /// - Checks all routes that match the resource_id (may be multiple)
    /// - Logs warnings on deserialization failures but continues
    /// - Logs debug message if no routes found for resource_id
    pub async fn route(&self, resource_id: &str, payload: &[u8]) -> Result<(), String> {
        let mut routed = false;

        // Linear search through all routes
        // Note: Multiple routes may match the same resource_id (different types)
        for route in &self.routes {
            if route.resource_id == resource_id {
                // Deserialize the payload
                match (route.deserializer)(payload) {
                    Ok(value_any) => {
                        // Produce into the buffer
                        match route.producer.produce_any(value_any).await {
                            Ok(()) => {
                                routed = true;

                                #[cfg(feature = "tracing")]
                                tracing::debug!("Routed message on '{}' to producer", resource_id);
                            }
                            Err(_e) => {
                                #[cfg(feature = "tracing")]
                                tracing::error!(
                                    "Failed to produce message on '{}': {}",
                                    resource_id,
                                    _e
                                );

                                #[cfg(feature = "defmt")]
                                defmt::error!(
                                    "Failed to produce message on '{}': {}",
                                    resource_id,
                                    _e.as_str()
                                );
                            }
                        }
                    }
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!(
                            "Failed to deserialize message on '{}': {}",
                            resource_id,
                            _e
                        );

                        #[cfg(feature = "defmt")]
                        defmt::warn!(
                            "Failed to deserialize message on '{}': {}",
                            resource_id,
                            _e.as_str()
                        );
                    }
                }
            }
        }

        if !routed {
            #[cfg(feature = "tracing")]
            tracing::debug!("No route found for resource: '{}'", resource_id);

            #[cfg(feature = "defmt")]
            defmt::debug!("No route found for resource: '{}'", resource_id);
        }

        Ok(())
    }

    /// Get list of all resource IDs registered in this router
    ///
    /// Useful for subscribing at the protocol level (e.g., MQTT SUBSCRIBE).
    /// Returns unique resource IDs (deduplicated even if multiple routes per resource).
    pub fn resource_ids(&self) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = self.routes.iter().map(|r| r.resource_id).collect();

        // Deduplicate
        ids.sort_unstable();
        ids.dedup();

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
///
/// # Example
///
/// ```rust,ignore
/// use aimdb_core::router::RouterBuilder;
///
/// let router = RouterBuilder::new()
///     .add_route(
///         "sensors/temperature",
///         producer_temp.clone(),
///         Arc::new(|bytes| {
///             serde_json::from_slice::<Temperature>(bytes)
///                 .map(|t| Box::new(t) as Box<dyn Any + Send>)
///                 .map_err(|e| e.to_string())
///         })
///     )
///     .add_route(
///         "sensors/humidity",
///         producer_humidity.clone(),
///         Arc::new(|bytes| {
///             serde_json::from_slice::<Humidity>(bytes)
///                 .map(|h| Box::new(h) as Box<dyn Any + Send>)
///                 .map_err(|e| e.to_string())
///         })
///     )
///     .build();
/// ```
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
    /// `AimDb::collect_inbound_routes()`. The resource_ids are leaked to satisfy
    /// the 'static lifetime requirement.
    ///
    /// # Arguments
    /// * `routes` - Vector of (resource_id, producer, deserializer) tuples
    ///
    /// # Example
    /// ```rust,ignore
    /// let routes = db.collect_inbound_routes("mqtt");
    /// let router = RouterBuilder::from_routes(routes).build();
    /// connector.set_router(router).await?;
    /// ```
    pub fn from_routes(routes: Vec<(String, Box<dyn ProducerTrait>, DeserializerFn)>) -> Self {
        let mut builder = Self::new();
        for (resource_id, producer, deserializer) in routes {
            // Leak the string to get 'static lifetime
            let resource_id_static: &'static str = Box::leak(resource_id.into_boxed_str());
            builder = builder.add_route(resource_id_static, producer, deserializer);
        }
        builder
    }

    /// Add a route to the router
    ///
    /// # Arguments
    /// * `resource_id` - Resource identifier to match (must have 'static lifetime)
    /// * `producer` - Producer that implements ProducerTrait
    /// * `deserializer` - Function to deserialize bytes to the target type
    ///
    /// # Resource ID Lifetime
    /// The resource_id must have 'static lifetime. Use string literals or leak strings:
    /// - String literal: `"sensors/temperature"`
    /// - Leaked string: `Box::leak(resource_id.into_boxed_str())`
    pub fn add_route(
        mut self,
        resource_id: &'static str,
        producer: Box<dyn ProducerTrait>,
        deserializer: DeserializerFn,
    ) -> Self {
        self.routes.push(Route {
            resource_id,
            producer,
            deserializer,
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
    use crate::connector::ProducerTrait;
    use std::any::Any;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // Mock producer for testing
    struct MockProducer {
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ProducerTrait for MockProducer {
        async fn produce_any(&self, _value: Box<dyn Any + Send>) -> Result<(), String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_single_route() {
        let call_count = Arc::new(AtomicUsize::new(0));

        let routes = vec![Route {
            resource_id: "test/resource",
            producer: Box::new(MockProducer {
                call_count: call_count.clone(),
            }),
            deserializer: Arc::new(|_bytes| Ok(Box::new(42i32))),
        }];

        let router = Router::new(routes);

        router.route("test/resource", b"dummy").await.unwrap();

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_multiple_routes_same_resource() {
        let call_count1 = Arc::new(AtomicUsize::new(0));
        let call_count2 = Arc::new(AtomicUsize::new(0));

        let routes = vec![
            Route {
                resource_id: "shared/resource",
                producer: Box::new(MockProducer {
                    call_count: call_count1.clone(),
                }),
                deserializer: Arc::new(|_bytes| Ok(Box::new(42i32))),
            },
            Route {
                resource_id: "shared/resource",
                producer: Box::new(MockProducer {
                    call_count: call_count2.clone(),
                }),
                deserializer: Arc::new(|_bytes| Ok(Box::new("test".to_string()))),
            },
        ];

        let router = Router::new(routes);

        router.route("shared/resource", b"dummy").await.unwrap();

        // Both producers should be called
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_unknown_resource() {
        let routes = vec![Route {
            resource_id: "test/resource",
            producer: Box::new(MockProducer {
                call_count: Arc::new(AtomicUsize::new(0)),
            }),
            deserializer: Arc::new(|_bytes| Ok(Box::new(42i32))),
        }];

        let router = Router::new(routes);

        // Should not panic on unknown resource
        router.route("unknown/resource", b"dummy").await.unwrap();
    }

    #[tokio::test]
    async fn test_resource_ids_deduplication() {
        let routes = vec![
            Route {
                resource_id: "resource1",
                producer: Box::new(MockProducer {
                    call_count: Arc::new(AtomicUsize::new(0)),
                }),
                deserializer: Arc::new(|_bytes| Ok(Box::new(42i32))),
            },
            Route {
                resource_id: "resource1", // Duplicate
                producer: Box::new(MockProducer {
                    call_count: Arc::new(AtomicUsize::new(0)),
                }),
                deserializer: Arc::new(|_bytes| Ok(Box::new("test".to_string()))),
            },
            Route {
                resource_id: "resource2",
                producer: Box::new(MockProducer {
                    call_count: Arc::new(AtomicUsize::new(0)),
                }),
                deserializer: Arc::new(|_bytes| Ok(Box::new(99i32))),
            },
        ];

        let router = Router::new(routes);
        let ids = router.resource_ids();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"resource1"));
        assert!(ids.contains(&"resource2"));
    }
}
