//! The `mountain-mqtt` backend: broker session plus the data-plane bridges.
//!
//! The session task drives both directions itself: it pulls outbound messages
//! from an [`OutboundRoutes`](aimdb_core::OutboundRoutes) when it can send,
//! and dispatches inbound publishes into their records through an
//! [`InboundDispatch`](aimdb_core::InboundDispatch). Core runs no pump for it.
//!
//! See the crate docs for a usage example.

pub(crate) mod manager;
pub(crate) mod session;

// The session's own machinery: incremental framing, and the three futures that
// replace the polled loop.
pub(crate) mod packet_reader;
pub(crate) mod session_loop;
pub(crate) mod write_ring;

// TLS transport + SNTP time source.
#[cfg(feature = "embassy-tls")]
pub mod sntp;
#[cfg(feature = "embedded-tls")]
pub mod tls;

extern crate alloc;

use aimdb_core::connector::ConnectorUrl;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

#[cfg(feature = "embassy-tls")]
use aimdb_embassy_adapter::connectors::into_box_future;

use mountain_mqtt::client::ConnectionSettings;

use crate::embedded::manager::Settings;
use crate::embedded::session_loop::{connect_packet, publish_frame_len, subscribe_len};
use crate::embedded::write_ring::{encoded_len, fits_ring, CONTROL_RESERVE};
use crate::publish_opts::PublishOpts;

pub(crate) use crate::embedded::write_ring::DEFAULT_WRITE_BUFFER;

#[cfg(feature = "embedded-tls")]
pub use crate::embedded::tls::TlsOptions;
#[cfg(feature = "embedded-tls")]
use crate::embedded::tls::{host_ip_literal, READ_BUF_MIN, WRITE_BUF_MIN};

/// Buffer size for MQTT packets (4KB)
pub(crate) const BUFFER_SIZE: usize = 4096;

/// Maximum properties on any received packet. Exceeding it ends the session, so
/// the headroom is for user properties on an inbound publish, which the
/// publishing peer chooses — broker CONNACKs use about ten.
pub(crate) const MAX_PROPERTIES: usize = 32;

/// The runner's collected future type.
type EmbassyBoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Force-`Send + Sync` slot for the TLS materials: [`TlsOptions`] holds
/// `&'static mut` exclusive resources, so it is neither `Sync` nor takeable
/// through the `&self` that [`ConnectorBuilder::build`] receives.
#[cfg(feature = "embedded-tls")]
pub(crate) type TlsSlot = aimdb_core::session::OneShot<TlsOptions>;

/// Connect and collect the data-plane futures for a plain `mqtt://` session.
pub(crate) fn build_plain<'a, D>(
    db: &'a aimdb_core::builder::AimDb,
    broker_url: &'a str,
    client_id: Option<&'a str>,
    credentials: Option<&'a (String, String)>,
    keep_alive_secs: u16,
    dialer: &'a D,
    write_buffer: usize,
) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<EmbassyBoxFuture>>> + Send + 'a>>
where
    D: aimdb_core::session::StreamDialer
        + aimdb_core::session::Delay
        + Clone
        + Send
        + Sync
        + 'static,
{
    Box::pin(async move {
        let inbound = aimdb_core::InboundDispatch::new(db, "mqtt", &crate::MqttGrammar)?;
        let topics = inbound_topics(&inbound);
        let broker = parse_broker_url(broker_url)?;
        if broker.tls {
            return Err(build_err("mqtts:// broker URLs require .tls(...)"));
        }
        let connection_settings =
            static_connection_settings(client_id, credentials, broker.credentials.as_ref());
        let settings = Settings::from_keep_alive_secs(keep_alive_secs);
        let (outbound, opts) =
            prepare_outbound(db, write_buffer, &settings, &connection_settings, &topics)?;

        setup_manager(
            &broker,
            connection_settings,
            dialer.clone(),
            topics,
            inbound,
            outbound,
            opts,
            write_buffer,
            settings,
            db.runtime_ops(),
        )
    })
}

/// Connect and collect the data-plane futures for an `mqtts://` session.
#[cfg(feature = "embedded-tls")]
pub(crate) fn build_tls<'a, D>(
    db: &'a aimdb_core::builder::AimDb,
    broker_url: &'a str,
    client_id: Option<&'a str>,
    credentials: Option<&'a (String, String)>,
    keep_alive_secs: u16,
    backend: &'a crate::connector::EmbeddedTls<D>,
) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<EmbassyBoxFuture>>> + Send + 'a>>
where
    D: aimdb_core::session::StreamDialer
        + aimdb_core::session::Delay
        + Clone
        + Send
        + Sync
        + 'static,
{
    Box::pin(async move {
        let inbound = aimdb_core::InboundDispatch::new(db, "mqtt", &crate::MqttGrammar)?;
        let topics = inbound_topics(&inbound);
        let broker = parse_broker_url(broker_url)?;
        if !broker.tls {
            return Err(build_err(".tls(...) requires an mqtts:// broker URL"));
        }
        let options = backend
            .options
            .take()
            .ok_or_else(|| build_err("TLS materials already taken; build() ran twice"))?;
        let connection_settings =
            static_connection_settings(client_id, credentials, broker.credentials.as_ref());
        let settings = Settings::from_keep_alive_secs(keep_alive_secs);
        let (outbound, opts) = prepare_outbound(
            db,
            backend.write_buffer,
            &settings,
            &connection_settings,
            &topics,
        )?;

        setup_tls_manager(
            &broker,
            options,
            connection_settings,
            backend.dialer.clone(),
            topics,
            inbound,
            outbound,
            opts,
            backend.write_buffer,
            settings,
            db.runtime_ops(),
        )
    })
}

/// The inbound topics the session must subscribe on every connection.
fn inbound_topics(inbound: &aimdb_core::InboundDispatch) -> Vec<String> {
    let topics: Vec<String> = inbound
        .subscriptions()
        .iter()
        .map(|t| t.to_string())
        .collect();

    #[cfg(feature = "defmt")]
    defmt::info!("MQTT: subscribing to {} inbound topics", topics.len());

    topics
}

/// Build the outbound routes and parse each route's options, and check that
/// every packet the session must be able to send fits the write ring: the
/// largest PUBLISH of every route, the CONNECT and each SUBSCRIBE. One that
/// does not would fail every session, and the connector would reconnect
/// forever. Warns once per route asking for `qos=2`, which this client sends
/// at QoS 1.
fn prepare_outbound(
    db: &aimdb_core::builder::AimDb,
    write_buffer: usize,
    settings: &Settings,
    connection_settings: &ConnectionSettings<'static>,
    topics: &[String],
) -> Result<(aimdb_core::OutboundRoutes, Vec<PublishOpts>), aimdb_core::DbError> {
    let outbound = aimdb_core::OutboundRoutes::new(db, "mqtt")?;
    let size = |len: Result<usize, crate::embedded::manager::Error>| {
        len.map_err(|_| build_err("a packet could not be sized"))
    };
    let mut problems: Vec<String> = Vec::new();
    let mut opts = Vec::with_capacity(outbound.routes().len());

    for route in outbound.routes() {
        match PublishOpts::parse(route) {
            Ok(opt) => {
                if opt.qos == 2 {
                    aimdb_core::log_warn!(
                        "MQTT: route '{}' asks for qos=2; this backend publishes it at QoS 1 (at-least-once). The std backend honours qos=2 on the same URL.",
                        route.default_topic
                    );
                    #[cfg(feature = "defmt")]
                    defmt::warn!(
                        "MQTT: route '{}' asks qos=2; publishing at QoS 1 (at-least-once)",
                        &*route.default_topic
                    );
                }
                opts.push(opt);
            }
            Err(e) => problems.push(e),
        }
        let topic_len = route.default_topic.len().max(route.topic_capacity);
        let frame = size(publish_frame_len(topic_len, route.payload_capacity))?;
        if !fits_ring(write_buffer, frame, CONTROL_RESERVE) {
            problems.push(format!(
                "route '{}': its largest PUBLISH is {frame} bytes, which needs a write buffer of at least {} bytes; it is {write_buffer}",
                route.default_topic,
                2 * (frame + CONTROL_RESERVE)
            ));
        }
    }

    let connect = size(encoded_len(&connect_packet(settings, connection_settings)))?;
    if !fits_ring(write_buffer, connect, 0) {
        problems.push(format!(
            "the CONNECT (client id and credentials) is {connect} bytes, which needs a write buffer of at least {} bytes; it is {write_buffer}",
            2 * connect
        ));
    }
    for topic in topics {
        let subscribe = size(subscribe_len(topic))?;
        if !fits_ring(write_buffer, subscribe, 0) {
            problems.push(format!(
                "the SUBSCRIBE to '{topic}' is {subscribe} bytes, which needs a write buffer of at least {} bytes; it is {write_buffer}",
                2 * subscribe
            ));
        }
    }

    if !problems.is_empty() {
        return Err(build_err(&problems.join("; ")));
    }
    Ok((outbound, opts))
}

/// Parsed broker endpoint: transport + authority.
struct BrokerUrl {
    tls: bool,
    host: String,
    port: u16,
    /// Credentials from the URL authority (`mqtt://user:pass@host`), which
    /// `MqttConnector::with_credentials` overrides.
    credentials: Option<(String, String)>,
}

fn build_err(msg: &str) -> aimdb_core::DbError {
    #[cfg(feature = "defmt")]
    defmt::error!("Failed to build MQTT connector: {}", msg);
    aimdb_core::DbError::runtime_error(format!("Failed to build MQTT connector: {}", msg))
}

/// Parse the broker URL into transport + host + port (`mqtt://` 1883,
/// `mqtts://` 8883), plus any credentials in the authority.
///
/// A username without a password is ignored rather than sent half-formed,
/// which is what the `rumqttc` backend does with the same URL.
fn parse_broker_url(broker_url: &str) -> Result<BrokerUrl, aimdb_core::DbError> {
    // Add a dummy topic if none, so parsing succeeds.
    let mut url = broker_url.to_string();
    if url.matches('/').count() < 3 {
        url = format!("{}/dummy", url.trim_end_matches('/'));
    }
    let connector_url = ConnectorUrl::parse(&url).map_err(|_| build_err("Invalid MQTT URL"))?;
    let tls = match connector_url.scheme.as_str() {
        "mqtt" => false,
        "mqtts" => true,
        _ => return Err(build_err("Broker URL scheme must be mqtt:// or mqtts://")),
    };
    let port = connector_url.port.unwrap_or(if tls { 8883 } else { 1883 });
    let credentials = match (connector_url.username, connector_url.password) {
        (Some(username), Some(password)) => Some((username, password)),
        _ => None,
    };
    Ok(BrokerUrl {
        tls,
        host: connector_url.host,
        port,
        credentials,
    })
}

/// Build the `ConnectionSettings<'static>` for MQTT CONNECT.
///
/// `credentials` is what the connector was given; `url_credentials` is what the
/// broker URL's authority carried. The explicit setter wins, as it does on the
/// `rumqttc` backend — it is the only way to name a password that is not
/// URL-safe.
///
/// The identity strings are leaked to reach `'static` — the session task is
/// `'static`, so what it borrows must be too — giving each connector its own
/// identity rather than a shared one.
///
/// The leak is per `build()` call, not per connector: three short strings, once
/// at startup, which is the normal case and indistinguishable from a static.
/// Only a process that rebuilds the database repeatedly accumulates them. The
/// alternative is owning the strings in the session task and rebuilding
/// `ConnectionSettings` per connection, which costs four signatures for memory
/// nobody misses.
fn static_connection_settings(
    client_id: Option<&str>,
    credentials: Option<&(String, String)>,
    url_credentials: Option<&(String, String)>,
) -> ConnectionSettings<'static> {
    fn leak(s: &str) -> &'static str {
        Box::leak(s.to_string().into_boxed_str())
    }

    let client_id = leak(client_id.unwrap_or("aimdb-client"));
    match credentials.or(url_credentials) {
        Some((username, password)) => {
            ConnectionSettings::authenticated(client_id, leak(username), leak(password).as_bytes())
        }
        None => ConnectionSettings::unauthenticated(client_id),
    }
}

/// Set up the plain-TCP broker session task, which drives both directions.
/// Synchronous — no `.await` — so the caller's `build` future stays `Send`.
#[allow(clippy::too_many_arguments)]
fn setup_manager<D>(
    broker: &BrokerUrl,
    connection_settings: ConnectionSettings<'static>,
    dialer: D,
    topics: Vec<String>,
    inbound: aimdb_core::InboundDispatch,
    outbound: aimdb_core::OutboundRoutes,
    opts: Vec<PublishOpts>,
    write_buffer: usize,
    settings: Settings,
    runtime: Arc<dyn aimdb_core::RuntimeOps>,
) -> aimdb_core::DbResult<Vec<EmbassyBoxFuture>>
where
    D: aimdb_core::session::StreamDialer
        + aimdb_core::session::Delay
        + Clone
        + Send
        + Sync
        + 'static,
{
    // The dialer is both the transport and the clock the session runs on.
    let host = broker.host.clone();
    let port = broker.port;

    // SAFETY: every value the session holds is `Send` — `StreamDialer`
    // guarantees `Stream: Send`, `InboundDispatch` and `OutboundRoutes` are
    // `Send`, and the session's channel is `CriticalSectionRawMutex`. See
    // `SendSession`.
    let manager_task: EmbassyBoxFuture = Box::pin(unsafe {
        crate::embedded::session::SendSession::new({
            async move {
                #[cfg(feature = "defmt")]
                defmt::info!("MQTT background task starting");

                crate::embedded::session::run_sessions(
                    dialer,
                    host,
                    port,
                    topics,
                    connection_settings,
                    settings,
                    inbound,
                    outbound,
                    opts,
                    write_buffer,
                    runtime,
                )
                .await
            }
        })
    });

    Ok(alloc::vec![manager_task])
}

/// Set up the TLS broker manager ([`run_tls`]) plus the SNTP time-source task.
/// Synchronous — no `.await` — so the caller's `build` future stays `Send`.
#[cfg(feature = "embedded-tls")]
#[allow(clippy::too_many_arguments)]
fn setup_tls_manager<D>(
    broker: &BrokerUrl,
    options: TlsOptions,
    connection_settings: ConnectionSettings<'static>,
    dialer: D,
    topics: Vec<String>,
    inbound: aimdb_core::InboundDispatch,
    outbound: aimdb_core::OutboundRoutes,
    opts: Vec<PublishOpts>,
    write_buffer: usize,
    settings: Settings,
    runtime: Arc<dyn aimdb_core::RuntimeOps>,
) -> aimdb_core::DbResult<Vec<EmbassyBoxFuture>>
where
    D: aimdb_core::session::StreamDialer
        + aimdb_core::session::Delay
        + Clone
        + Send
        + Sync
        + 'static,
{
    match host_ip_literal(&broker.host) {
        Some(core::net::IpAddr::V6(_)) => {
            return Err(build_err(
                "mqtts:// with an IPv6 literal can never pass certificate verification — use a hostname",
            ));
        }
        Some(core::net::IpAddr::V4(_)) => {
            // Verifies only via the certificate's CN — private-CA bench
            // setups pin the IP there; public CAs won't issue such certs.
            #[cfg(feature = "defmt")]
            defmt::warn!(
                "MQTT-TLS: broker host is an IP literal; the certificate must carry it in CN — prefer a hostname"
            );
        }
        None => {}
    }
    if options.read_buf.len() < READ_BUF_MIN {
        return Err(build_err(
            "TLS read buffer too small — a TLS 1.3 peer may send 16 KB records; provide at least 16 640 bytes",
        ));
    }
    if options.write_buf.len() < WRITE_BUF_MIN {
        return Err(build_err(
            "TLS write buffer too small — each record costs 128 bytes of overhead, and below that the writer runs off the end of the buffer; provide at least 256 bytes, or 4 096 for MQTT-sized writes in one record",
        ));
    }

    let host = broker.host.clone();
    let port = broker.port;
    #[cfg(feature = "embassy-tls")]
    let sntp = options.sntp;

    let delay = dialer.clone();
    // SAFETY: as for the plain path — `StreamDialer` guarantees `Stream: Send`,
    // `InboundDispatch` and `OutboundRoutes` are `Send`, and `TlsOptions` is
    // `Send` (its RNG carries the bound). See `session::SendSession`.
    #[cfg_attr(not(feature = "embassy-tls"), allow(unused_mut))]
    let mut tasks: Vec<EmbassyBoxFuture> = alloc::vec![Box::pin(unsafe {
        crate::embedded::session::SendSession::new({
            async move {
                #[cfg(feature = "defmt")]
                defmt::info!("MQTT-TLS background task starting");

                #[allow(unreachable_code)]
                {
                    let _: () = crate::embedded::tls::run_tls(
                        dialer,
                        options,
                        host,
                        port,
                        topics,
                        connection_settings,
                        settings,
                        inbound,
                        outbound,
                        opts,
                        write_buffer,
                        delay,
                        runtime,
                    )
                    .await;
                }
            }
        })
    }) as EmbassyBoxFuture];

    // Only a runtime with no wall clock of its own needs this.
    #[cfg(feature = "embassy-tls")]
    if let Some((stack, server)) = sntp {
        tasks.push(into_box_future(async move {
            #[allow(unreachable_code)]
            {
                let _: () = crate::embedded::sntp::run(*stack.get(), server).await;
            }
        }));
    }

    Ok(tasks)
}
