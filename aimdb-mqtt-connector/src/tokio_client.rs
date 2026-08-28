//! MQTT client management and lifecycle
//!
//! This module provides a client pool that:
//! - Manages a single MQTT broker connection
//! - Automatic event loop spawning
//! - Thread-safe access from multiple consumers
//! - Explicit lifecycle management (user controls when clients are created)

use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::{Router, RouterBuilder};
use aimdb_core::transport::{Connector, ConnectorConfig, PublishError};
use aimdb_core::{log_debug, log_error, log_info};
use aimdb_core::{pump_sink, pump_source, BoxFut, ConnectorBuilder, Payload, Source};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// MQTT connector for a single broker connection with router-based dispatch
///
/// Each connector manages ONE MQTT broker connection. The router determines
/// how incoming messages are dispatched to AimDB producers.
///
/// # Usage Pattern
///
/// The connector collects routes from the database during build() and
/// automatically subscribes to all required MQTT topics.
pub struct MqttConnectorBuilder {
    broker_url: String,
    client_id: Option<String>,
}

impl MqttConnectorBuilder {
    /// Create a new MQTT connector builder
    ///
    /// If no client ID is explicitly set via `with_client_id()`, a random
    /// UUID-based client ID will be generated automatically when the connector
    /// is built.
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL (mqtt://host:port or mqtts://host:port)
    pub fn new(broker_url: impl Into<String>) -> Self {
        Self {
            broker_url: broker_url.into(),
            client_id: None,
        }
    }

    /// Set the MQTT client ID
    ///
    /// The client ID should be unique for each client connecting to the broker.
    /// It's used for session persistence and message delivery guarantees.
    ///
    /// If not set, a random UUID-based client ID will be generated automatically.
    ///
    /// # Arguments
    /// * `client_id` - Unique identifier for this client
    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = Some(client_id.into());
        self
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

impl ConnectorBuilder for MqttConnectorBuilder {
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb,
    ) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<BoxFuture>>> + Send + 'a>> {
        Box::pin(async move {
            // Build a router from the inbound routes purely to drive the MQTT
            // subscriptions + channel-capacity sizing in `build_internal`. The
            // routing `Router` that fans incoming frames out to producers is
            // (re)built by `pump_source` from the same `collect_inbound_routes`.
            let inbound_routes = db.collect_inbound_routes("mqtt");
            let router = RouterBuilder::from_routes(inbound_routes).build();

            log_info!("MQTT subscribing to {} topics", router.resource_ids().len());

            // Connect, subscribe, and hand back the raw event loop.
            let (client, event_loop) =
                MqttConnectorImpl::build_internal(&self.broker_url, self.client_id.clone(), router)
                    .await
                    .map_err(|e| {
                        aimdb_core::DbError::runtime_error(format!(
                            "Failed to build MQTT connector: {}",
                            e
                        ))
                    })?;

            let mut futures: Vec<BoxFuture> = Vec::new();

            // Inbound: one multiplexed reader future fanning publishes out to producers.
            futures.extend(pump_source(
                db,
                "mqtt",
                MqttEventLoopSource {
                    event_loop,
                    broker_key: self.broker_url.clone(),
                },
            ));

            // Outbound: one publisher future per outbound route.
            futures.extend(pump_sink(db, "mqtt", Arc::new(MqttSink { client })));

            Ok(futures)
        })
    }

    fn scheme(&self) -> &str {
        "mqtt"
    }
}

/// Internal MQTT connector build helpers.
///
/// A namespace for the broker-connection setup invoked from
/// [`MqttConnectorBuilder::build`]; the data-plane loops themselves live in the
/// reusable `pump_sink` / `pump_source` helpers + the [`MqttSink`] /
/// [`MqttEventLoopSource`] adapters below.
pub struct MqttConnectorImpl;

impl MqttConnectorImpl {
    /// Connect to the broker and subscribe to all configured topics (internal).
    ///
    /// Creates the MQTT client, sizes the send-channel from the route count, and
    /// subscribes to every topic in `router`. Returns the shared client (for the
    /// outbound `pump_sink`) plus the raw event loop (handed to a
    /// [`MqttEventLoopSource`] for the inbound `pump_source`).
    ///
    /// # Arguments
    /// * `broker_url` - Broker URL (mqtt://host:port or mqtts://host:port)
    /// * `client_id` - Optional client ID (if None, generates UUID-based ID)
    /// * `router` - Routes used only for the subscription list + capacity sizing
    async fn build_internal(
        broker_url: &str,
        client_id: Option<String>,
        router: Router,
    ) -> Result<(Arc<AsyncClient>, EventLoop), String> {
        // Parse the broker URL - we accept it with or without a topic
        let mut url = broker_url.to_string();

        // If no topic is provided, add a dummy one for parsing
        if !url.contains('/') || url.matches('/').count() < 3 {
            url = format!("{}/dummy", url.trim_end_matches('/'));
        }

        let connector_url =
            ConnectorUrl::parse(&url).map_err(|e| format!("Invalid MQTT URL: {}", e))?;

        let host = connector_url.host.clone();
        let port = connector_url.port.unwrap_or_else(|| {
            if connector_url.scheme == "mqtts" {
                8883
            } else {
                1883
            }
        });

        log_info!("Creating MQTT client for {}:{}", host, port);

        // Use provided client_id or generate a UUID-based one
        let client_id = client_id.unwrap_or_else(|| format!("aimdb-{}", uuid::Uuid::new_v4()));

        let mut mqtt_opts = MqttOptions::new(client_id, host, port);

        mqtt_opts.set_keep_alive(Duration::from_secs(30));

        // Add credentials if provided
        if let (Some(ref username), Some(ref password)) =
            (&connector_url.username, &connector_url.password)
        {
            mqtt_opts.set_credentials(username, password);
        }

        // mqtts:// selects the TLS transport; rumqttc otherwise speaks plain TCP
        // regardless of port. Which stack answers is a build-time choice.
        //
        // The whole branch is gated, not just the configuration: rumqttc gates
        // `TlsConfiguration` *and* `Transport::Tls` on having a backend, so a
        // build with neither cannot even name the types.
        #[cfg(any(feature = "tokio-native-tls", feature = "tokio-rustls"))]
        if connector_url.scheme == "mqtts" {
            mqtt_opts.set_transport(rumqttc::Transport::Tls(tls_configuration()?));
        }
        #[cfg(not(any(feature = "tokio-native-tls", feature = "tokio-rustls")))]
        if connector_url.scheme == "mqtts" {
            return Err(no_tls_backend());
        }

        // Wrap router early so we can count topics for capacity calculation
        let router_arc = Arc::new(router);
        let topic_count = router_arc.resource_ids().len();

        // Dynamic channel capacity: scales with topic count.
        //
        // With spawn-before-subscribe, the event loop drains continuously, so the
        // client send buffer only needs a small fixed headroom to absorb short
        // bursts of publishes and QoS handshake packets (PUBACK/PUBREC/PUBREL/PUBCOMP).
        //
        // A value of 10 has been chosen empirically as a conservative upper bound
        // for typical burst sizes in this connector without over-allocating, while
        // still keeping backpressure behavior predictable.
        const CHANNEL_HEADROOM: usize = 10;
        let channel_capacity = topic_count + CHANNEL_HEADROOM;

        log_debug!(
            "MQTT channel capacity set to {} (for {} topics)",
            channel_capacity,
            topic_count
        );

        // Create client and event loop with dynamic capacity
        let (client, event_loop) = AsyncClient::new(mqtt_opts, channel_capacity);
        let client_arc = Arc::new(client);

        let topics = router_arc.resource_ids();

        log_info!("Subscribing to {} MQTT topics...", topics.len());

        for topic in &topics {
            log_debug!("Subscribing to MQTT topic: {}", topic);

            client_arc
                .subscribe(topic.as_ref(), rumqttc::QoS::AtLeastOnce)
                .await
                .map_err(|e| format!("Failed to subscribe to topic '{}': {}", topic, e))?;
        }

        log_info!("MQTT subscriptions complete");

        Ok((client_arc, event_loop))
    }
}

/// Pure outbound publish adapter driven by `pump_sink`.
///
/// Wraps the shared rumqttc client. `qos`/`retain` come from the route's protocol
/// options (threaded through by `pump_sink` via [`ConnectorConfig::from_query`]),
/// interpreted with MQTT's legacy defaults — **QoS 1 (`AtLeastOnce`)** when
/// unspecified, no retain — so the wire stays byte-identical to the old loop.
struct MqttSink {
    client: Arc<AsyncClient>,
}

impl MqttSink {
    /// Look up a protocol option by key and parse it.
    fn opt<T: core::str::FromStr>(config: &ConnectorConfig, key: &str) -> Option<T> {
        config
            .protocol_options
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.parse().ok())
    }
}

impl Connector for MqttSink {
    fn publish(
        &self,
        destination: &str,
        config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        // Legacy defaults: QoS 1 when no `qos` query option, no retain.
        let qos = Self::opt::<u8>(config, "qos").unwrap_or(1);
        let retain = Self::opt::<bool>(config, "retain").unwrap_or(false);

        // Destination is already the MQTT topic (from ConnectorUrl::resource_id()).
        let topic = destination.to_string();
        let payload_owned = payload.to_vec();
        let client = self.client.clone();

        Box::pin(async move {
            let qos_level = match qos {
                0 => rumqttc::QoS::AtMostOnce,
                1 => rumqttc::QoS::AtLeastOnce,
                2 => rumqttc::QoS::ExactlyOnce,
                _ => return Err(PublishError::UnsupportedQoS),
            };

            // Borrowed before `topic` is moved into `publish`, which is why this
            // reads "Publishing" and sits above the call: the alternative was a
            // `String` clone on every publish just to name the topic afterwards.
            // A failed publish is reported by the `map_err` below.
            log_debug!("Publishing to topic: {}", topic);

            client
                .publish(topic, qos_level, retain, payload_owned)
                .await
                .map_err(|_e| {
                    log_error!("MQTT publish failed: {}", _e);

                    PublishError::ConnectionFailed
                })?;

            Ok(())
        })
    }
}

/// Inbound frame source driven by `pump_source`.
///
/// Yields `(topic, payload)` for each incoming MQTT publish. The inner poll loop
/// discards non-publish packets — keeping QoS handshakes and keepalive flowing —
/// and backs off 5s on a connection error before retrying, reproducing the old
/// hand-rolled event-loop future exactly. It never yields `None`: the reader runs
/// for the lifetime of the connector.
struct MqttEventLoopSource {
    event_loop: EventLoop,
    /// Only ever used to name the broker in an error line. One `String` per
    /// connection, held for its lifetime — no longer feature-gated, because the
    /// facade decides its own gating and a `#[cfg]` here could not follow it.
    broker_key: String,
}

impl Source for MqttEventLoopSource {
    fn next(&mut self) -> BoxFut<'_, Option<(String, Payload)>> {
        Box::pin(async move {
            loop {
                match self.event_loop.poll().await {
                    Ok(Event::Incoming(Packet::Publish(publish))) => {
                        let topic = publish.topic.clone();
                        let payload: Payload = Arc::from(publish.payload.as_ref());

                        log_debug!(
                            "Received MQTT message on topic '{}' ({} bytes)",
                            topic,
                            payload.len()
                        );

                        return Some((topic, payload));
                    }
                    // Non-publish packets (PUBACK/PINGRESP/…) keep driving the protocol.
                    Ok(_) => continue,
                    Err(_e) => {
                        log_error!("MQTT event loop error for {}: {:?}", self.broker_key, _e);

                        // Wait before reconnecting.
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        })
    }
}

/// The TLS configuration for `mqtts://`, from whichever backend this build
/// selected.
#[cfg(feature = "tokio-native-tls")]
fn tls_configuration() -> Result<rumqttc::TlsConfiguration, String> {
    Ok(rumqttc::TlsConfiguration::Native)
}

/// Built by hand rather than via `TlsConfiguration::default()`, which does the
/// same work and then `expect`s on failure. A panic on the connect path is
/// undefined behaviour across an FFI boundary; a returned error is a status.
#[cfg(all(feature = "tokio-rustls", not(feature = "tokio-native-tls")))]
fn tls_configuration() -> Result<rumqttc::TlsConfiguration, String> {
    use rumqttc::tokio_rustls::rustls::{ClientConfig, RootCertStore};

    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        // A trust store with one unparseable certificate is still a trust
        // store; refusing the lot would be worse than skipping the entry.
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err("no usable platform trust roots were found, so no broker \
                    certificate could be verified"
            .to_string());
    }

    Ok(rumqttc::TlsConfiguration::Rustls(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )))
}

/// Why an `mqtts://` url cannot be honoured by a build with no TLS backend.
#[cfg(not(any(feature = "tokio-native-tls", feature = "tokio-rustls")))]
fn no_tls_backend() -> String {
    "this build has no TLS backend — rebuild aimdb-mqtt-connector with the \
     `tokio-rustls` or `tokio-native-tls` feature, or use an mqtt:// url"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::router::RouterBuilder;

    #[tokio::test]
    async fn test_connector_creation_with_router() {
        let router = RouterBuilder::new().build();
        let connector =
            MqttConnectorImpl::build_internal("mqtt://localhost:1883", None, router).await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_connector_with_port() {
        let router = RouterBuilder::new().build();
        let connector =
            MqttConnectorImpl::build_internal("mqtt://broker.local:9999", None, router).await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_url() {
        let router = RouterBuilder::new().build();
        let connector = MqttConnectorImpl::build_internal("not-a-valid-url", None, router).await;
        assert!(connector.is_err());
    }

    #[tokio::test]
    async fn test_connector_mqtts_url_with_credentials() {
        // mqtts:// with URL-embedded credentials must parse and build; the TLS
        // handshake itself only happens once the event loop is polled.
        let router = RouterBuilder::new().build();
        let connector = MqttConnectorImpl::build_internal(
            "mqtts://hub-sub:secret@broker.example.com:8883",
            None,
            router,
        )
        .await;

        #[cfg(any(feature = "tokio-native-tls", feature = "tokio-rustls"))]
        assert!(connector.is_ok());

        // With no backend selected there is no TLS stack to hand the transport,
        // so mqtts:// is refused here rather than at the linker.
        #[cfg(not(any(feature = "tokio-native-tls", feature = "tokio-rustls")))]
        {
            // `Err(_)` rather than `expect_err`: the Ok half holds an
            // `EventLoop`, which is not `Debug`.
            let Err(err) = connector else {
                panic!("mqtts:// must be refused when no TLS backend is selected");
            };
            assert!(
                err.contains("no TLS backend"),
                "the error should name the missing feature, got: {err}"
            );
        }
    }

    /// The plain scheme is unaffected by which backend, if any, is selected.
    #[tokio::test]
    async fn test_connector_mqtt_url_needs_no_tls_backend() {
        let router = RouterBuilder::new().build();
        let connector =
            MqttConnectorImpl::build_internal("mqtt://broker.example.com:1883", None, router).await;
        assert!(connector.is_ok());
    }
}
