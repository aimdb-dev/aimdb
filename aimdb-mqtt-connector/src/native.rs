//! The `rumqttc` backend: one broker connection, QoS 0–2, platform trust roots.
//!
//! `rumqttc` owns its socket, TLS and reconnect, so this module contributes
//! the connect-and-subscribe step and two tasks: the event loop, which
//! dispatches inbound publishes into their records, and the publish loop,
//! which pulls outbound messages and hands them to `rumqttc`.

use aimdb_core::connector::ConnectorUrl;
use aimdb_core::{log_debug, log_error, log_info};
use aimdb_core::{InboundDispatch, OutboundRoutes};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::publish_opts::PublishOpts;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Connect, subscribe, and collect the data-plane futures for the `rumqttc`
/// backend.
pub(crate) fn build<'a>(
    db: &'a aimdb_core::builder::AimDb,
    broker_url: &'a str,
    client_id: Option<&'a str>,
    credentials: Option<&'a (String, String)>,
    keep_alive_secs: u16,
) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<BoxFuture>>> + Send + 'a>> {
    Box::pin(async move {
        // One dispatcher both subscribes (here) and routes (the event loop).
        let inbound = InboundDispatch::new(db, "mqtt", &crate::MqttGrammar)?;
        let topics = inbound.subscriptions();

        // Routes are subscribed now, so nothing produced before the publish
        // loop first runs is missed; options are parsed once, here.
        let outbound = OutboundRoutes::new(db, "mqtt")?;
        let opts = outbound
            .routes()
            .iter()
            .map(PublishOpts::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                aimdb_core::DbError::runtime_error(format!("Failed to build MQTT connector: {e}"))
            })?;

        log_info!("MQTT subscribing to {} topics", topics.len());

        // Connect, subscribe, and hand back the raw event loop.
        let (client, event_loop) = MqttConnectorImpl::build_internal(
            broker_url,
            client_id,
            credentials,
            keep_alive_secs,
            &topics,
        )
        .await
        .map_err(|e| {
            aimdb_core::DbError::runtime_error(format!("Failed to build MQTT connector: {}", e))
        })?;

        let futures: Vec<BoxFuture> = vec![
            Box::pin(run_event_loop(event_loop, inbound, broker_url.to_string())),
            Box::pin(run_publish_loop(client, outbound, opts)),
        ];
        Ok(futures)
    })
}

/// The broker-connection setup invoked from `build`.
pub struct MqttConnectorImpl;

impl MqttConnectorImpl {
    /// Connect to the broker and subscribe to `topics`, sizing the send
    /// channel from their count.
    ///
    /// Returns the client (for the publish loop) plus the raw event loop.
    /// A `None` `client_id` generates a UUID-based one.
    async fn build_internal(
        broker_url: &str,
        client_id: Option<&str>,
        credentials: Option<&(String, String)>,
        keep_alive_secs: u16,
        topics: &[Arc<str>],
    ) -> Result<(AsyncClient, EventLoop), String> {
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
        let client_id = client_id
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("aimdb-{}", uuid::Uuid::new_v4()));

        let mut mqtt_opts = MqttOptions::new(client_id, host, port);

        // The same promise the embedded backend makes, from the same setter:
        // the two backends used to disagree here (30 s against 60 s) for one
        // route URL.
        mqtt_opts.set_keep_alive(Duration::from_secs(keep_alive_secs.into()));

        // `with_credentials` wins over anything in the URL's authority, which
        // is the only way to name a password that is not URL-safe.
        match (
            credentials,
            &connector_url.username,
            &connector_url.password,
        ) {
            (Some((username, password)), _, _) => {
                mqtt_opts.set_credentials(username, password);
            }
            (None, Some(username), Some(password)) => {
                mqtt_opts.set_credentials(username, password);
            }
            _ => {}
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

        let topic_count = topics.len();

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

        log_info!("Subscribing to {} MQTT topics...", topics.len());

        for topic in topics {
            log_debug!("Subscribing to MQTT topic: {}", topic);

            client
                .subscribe(topic.as_ref(), rumqttc::QoS::AtLeastOnce)
                .await
                .map_err(|e| format!("Failed to subscribe to topic '{}': {}", topic, e))?;
        }

        log_info!("MQTT subscriptions complete");

        Ok((client, event_loop))
    }
}

/// Drive `rumqttc`'s event loop, dispatching every inbound publish into its
/// records. Other packets (PUBACK, PINGRESP, …) only keep the protocol going;
/// a connection error backs off 5 s before `rumqttc` reconnects. Never
/// returns: the loop runs for the lifetime of the connector.
///
/// `_broker_key` only names the broker in an error line; the logging facade
/// decides whether that line exists.
async fn run_event_loop(mut event_loop: EventLoop, inbound: InboundDispatch, _broker_key: String) {
    loop {
        match event_loop.poll().await {
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                log_debug!(
                    "Received MQTT message on topic '{}' ({} bytes)",
                    publish.topic,
                    publish.payload.len()
                );
                inbound.dispatch(&publish.topic, &publish.payload);
            }
            Ok(_) => {}
            Err(_e) => {
                log_error!("MQTT event loop error for {}: {:?}", _broker_key, _e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Pull outbound messages and hand each to `rumqttc` with its route's QoS and
/// retain flag. `publish` takes an owned topic and payload, so the topic is
/// copied and a borrowed payload copied (an owned one is moved). It waits for
/// room in `rumqttc`'s request channel, which is the backpressure. A failed
/// publish is logged with its topic and counted as rejected in the route's
/// `RouteStats`. Returns once every outbound route has closed.
async fn run_publish_loop(
    client: AsyncClient,
    mut outbound: OutboundRoutes,
    opts: Vec<PublishOpts>,
) {
    while let Some(msg) = outbound.next().await {
        let id = msg.route.id;
        let opt = opts.get(id).copied().unwrap_or(PublishOpts {
            qos: 1,
            retain: false,
        });
        let qos = match opt.qos {
            0 => rumqttc::QoS::AtMostOnce,
            2 => rumqttc::QoS::ExactlyOnce,
            _ => rumqttc::QoS::AtLeastOnce,
        };
        let topic = msg.topic.to_string();
        let payload = msg.payload.into_vec();

        log_debug!("Publishing to topic: {}", topic);
        if let Err(_e) = client.publish(topic, qos, opt.retain, payload).await {
            // The topic moved into `publish`; the route's default names it.
            log_error!(
                "MQTT publish on route '{}' failed: {}",
                outbound.routes()[id].default_topic,
                _e
            );
            outbound.reject(id);
        }
    }
    log_info!("MQTT publish loop: every outbound route has closed");
}

/// The TLS configuration for `mqtts://`, from whichever backend this build
/// selected.
#[cfg(feature = "tokio-native-tls")]
fn tls_configuration() -> Result<rumqttc::TlsConfiguration, String> {
    Ok(rumqttc::TlsConfiguration::Native)
}

/// Built by hand rather than via `TlsConfiguration::default()`, which `expect`s
/// on failure: a panic on the connect path is undefined behaviour across an FFI
/// boundary.
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

    #[tokio::test]
    async fn test_connector_creation_with_router() {
        let connector =
            MqttConnectorImpl::build_internal("mqtt://localhost:1883", None, None, 60, &[]).await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_connector_with_port() {
        let connector =
            MqttConnectorImpl::build_internal("mqtt://broker.local:9999", None, None, 60, &[])
                .await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_url() {
        let connector =
            MqttConnectorImpl::build_internal("not-a-valid-url", None, None, 60, &[]).await;
        assert!(connector.is_err());
    }

    #[tokio::test]
    async fn test_connector_mqtts_url_with_credentials() {
        // mqtts:// with URL-embedded credentials must parse and build; the TLS
        // handshake itself only happens once the event loop is polled.
        let connector = MqttConnectorImpl::build_internal(
            "mqtts://hub-sub:secret@broker.example.com:8883",
            None,
            None,
            60,
            &[],
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

    /// A database with `routes` outbound records on `mqtt://out/{i}`, each
    /// link carrying `config`, built with the native connector.
    async fn native_db(
        routes: usize,
        config: &'static [(&'static str, &'static str)],
    ) -> aimdb_core::DbResult<aimdb_core::AimDb> {
        use aimdb_core::buffer::BufferCfg;
        use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

        let mut builder = aimdb_core::AimDbBuilder::new()
            .runtime(Arc::new(TokioAdapter))
            .with_connector(crate::MqttConnector::new("mqtt://127.0.0.1:1"));
        for i in 0..routes {
            let key = aimdb_core::StringKey::intern(format!("out{i}"));
            builder.configure::<u64>(key, move |reg| {
                let mut link = reg
                    .buffer(BufferCfg::SingleLatest)
                    .link_to(&format!("mqtt://out/{i}"))
                    .with_serializer(|_ctx, v: &u64| Ok(v.to_le_bytes().to_vec()));
                for (k, v) in config {
                    link = link.with_config(k, v);
                }
                link.finish();
            });
        }
        builder.build().await.map(|(db, _runner)| db)
    }

    #[tokio::test]
    async fn an_invalid_qos_or_retain_fails_the_build() {
        for (config, expected) in [
            (&[("qos", "3")][..], "qos must be 0, 1 or 2, got '3'"),
            (&[("qos", "abc")][..], "qos must be 0, 1 or 2, got 'abc'"),
            (
                &[("retain", "yes")][..],
                "retain must be true or false, got 'yes'",
            ),
        ] {
            let Err(err) = native_db(1, config).await else {
                panic!("{config:?} must fail the build");
            };
            let err = err.to_string();
            assert!(err.contains("route 'out/0'"), "{err}");
            assert!(err.contains(expected), "{err}");
        }
        // qos=2 is honoured natively, not refused.
        assert!(native_db(1, &[("qos", "2"), ("retain", "true")])
            .await
            .is_ok());
    }

    /// One event loop and one publish loop, however many routes: no
    /// per-route pump.
    #[tokio::test]
    async fn the_native_backend_runs_two_tasks_whatever_the_route_count() {
        use aimdb_core::connector::ConnectorBuilder;

        for routes in [0, 1, 5] {
            let db = native_db(routes, &[]).await.expect("build");
            let futures = crate::MqttConnector::new("mqtt://127.0.0.1:1")
                .build(&db)
                .await
                .expect("build connector");
            assert_eq!(futures.len(), 2, "{routes} routes");
        }
    }

    /// The plain scheme is unaffected by which backend, if any, is selected.
    #[tokio::test]
    async fn test_connector_mqtt_url_needs_no_tls_backend() {
        let connector = MqttConnectorImpl::build_internal(
            "mqtt://broker.example.com:1883",
            None,
            None,
            60,
            &[],
        )
        .await;
        assert!(connector.is_ok());
    }
}
