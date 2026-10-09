//! The `zenoh` crate backend: one session task that subscribes, dispatches
//! inbound samples from the subscriber callbacks, and pulls outbound
//! messages.

use std::boxed::Box;
use std::format;
use std::string::{String, ToString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::vec::Vec;

use aimdb_core::{
    log_debug, log_error, log_info, log_warn, AimDb, DbError, DbResult, InboundDispatch,
    OutboundRoutes, RouteInfo, TopicFilter, TopicGrammar, TopicPattern, MAX_CAPTURES,
};
use zenoh::key_expr::{keyexpr, KeyExpr};
use zenoh::pubsub::Publisher;
use zenoh::sample::{Locality, SampleKind};
use zenoh::Session;

use crate::connector::{BoxFuture, BuildFuture};
use crate::{ZenohGrammar, SCHEME};

/// The first wait before retrying to open the session; it doubles up to
/// [`OPEN_RETRY_MAX`] while no router answers.
const OPEN_RETRY: Duration = Duration::from_secs(1);
const OPEN_RETRY_MAX: Duration = Duration::from_secs(30);

/// Validate every route and return the session task.
pub(crate) fn build<'a>(
    db: &'a AimDb,
    endpoint: &'a str,
    config: Option<zenoh::Config>,
) -> BuildFuture<'a> {
    Box::pin(async move {
        let inbound = InboundDispatch::new(db, SCHEME, &ZenohGrammar)?;
        // Subscribed now, so values produced before the session opens are kept.
        let outbound = OutboundRoutes::new(db, SCHEME)?;
        let keys = outbound
            .routes()
            .iter()
            .map(route_key)
            .collect::<Result<Vec<_>, _>>()?;
        let config = session_config(endpoint, config)?;
        let filters = subscriptions(&inbound)?;

        log_info!(
            "Zenoh: {} subscriptions, {} outbound routes",
            filters.len(),
            keys.len()
        );
        let task: BoxFuture = Box::pin(run(config, inbound, filters, outbound, keys));
        Ok(Vec::from([task]))
    })
}

pub(crate) fn config_error(message: String) -> DbError {
    DbError::runtime_error(format!("Failed to build Zenoh connector: {message}"))
}

/// The key of a route's link URL. Outbound keys hold no wildcards: a `put` on
/// `a/*` would reach every intersecting subscriber, and AimDB peers drop such
/// samples.
fn route_key(route: &RouteInfo) -> DbResult<KeyExpr<'static>> {
    let topic = &*route.default_topic;
    let key = KeyExpr::try_from(topic.to_string())
        .map_err(|e| config_error(format!("'{topic}' is not a key expression: {e}")))?;
    if topic.contains('*') {
        return Err(config_error(format!(
            "outbound key '{topic}' must not hold a wildcard"
        )));
    }
    Ok(key)
}

/// Zenoh's defaults in client mode, or the caller's configuration, with
/// `endpoint` as the router to connect to.
pub(crate) fn session_config(
    endpoint: &str,
    config: Option<zenoh::Config>,
) -> DbResult<zenoh::Config> {
    let mut config = match config {
        Some(config) => config,
        None => {
            let mut config = zenoh::Config::default();
            config
                .insert_json5("mode", r#""client""#)
                .map_err(|e| config_error(format!("mode: {e}")))?;
            config
        }
    };
    if !endpoint.is_empty() {
        if endpoint.contains(['"', '\\']) {
            return Err(config_error(format!("invalid endpoint '{endpoint}'")));
        }
        config
            .insert_json5("connect/endpoints", &format!(r#"["{endpoint}"]"#))
            .map_err(|e| config_error(format!("endpoint '{endpoint}': {e}")))?;
    }
    Ok(config)
}

/// The filters to subscribe, each compiled to tell which earlier subscriber
/// also delivers a sample.
fn subscriptions(
    inbound: &InboundDispatch,
) -> DbResult<Vec<(KeyExpr<'static>, Box<dyn TopicFilter>)>> {
    inbound
        .subscriptions()
        .iter()
        .map(|filter| {
            let key = KeyExpr::try_from(filter.to_string())
                .map_err(|e| config_error(format!("filter '{filter}': {e}")))?;
            let compiled = TopicPattern::parse(filter)
                .map_err(|e| e.to_string())
                .and_then(|pattern| ZenohGrammar.compile(&pattern))
                .map_err(|e| config_error(format!("filter '{filter}': {e}")))?;
            Ok((key, compiled))
        })
        .collect()
}

/// Open the session, subscribe, then publish until every outbound route has
/// closed. Zenoh reconnects and re-declares on its own once the session is
/// open.
///
/// Subscribers and publishers are remote-only: a `put` never reaches this
/// session's own subscribers. Otherwise a record linked to and from one key
/// would ingest its own publications in a loop, and an inbound link would see
/// the database's own writes.
async fn run(
    config: zenoh::Config,
    inbound: InboundDispatch,
    filters: Vec<(KeyExpr<'static>, Box<dyn TopicFilter>)>,
    mut outbound: OutboundRoutes,
    keys: Vec<KeyExpr<'static>>,
) {
    let session = open(config).await;

    let _subscribers = subscribe(&session, &inbound, filters).await;
    let mut publishers: Vec<Option<Publisher<'static>>> = Vec::with_capacity(keys.len());
    for key in keys {
        match session
            .declare_publisher(key)
            .allowed_destination(Locality::Remote)
            .await
        {
            Ok(publisher) => publishers.push(Some(publisher)),
            Err(_e) => {
                log_error!("Zenoh: cannot declare a publisher: {}", _e);
                publishers.push(None);
            }
        }
    }

    while let Some(msg) = outbound.next().await {
        let id = msg.route.id;
        let written = msg.topic != &*msg.route.default_topic;
        let result = match (&publishers[id], written) {
            (Some(publisher), false) => publisher.put(msg.payload.into_vec()).await,
            _ => match written_key(msg.topic) {
                Some(key) => {
                    session
                        .put(key, msg.payload.into_vec())
                        .allowed_destination(Locality::Remote)
                        .await
                }
                None => {
                    log_error!("Zenoh: '{}' is not a key without wildcards", msg.topic);
                    outbound.reject(id);
                    continue;
                }
            },
        };
        if let Err(_e) = result {
            log_error!(
                "Zenoh: put on route '{}' failed: {}",
                outbound.routes()[id].default_topic,
                _e
            );
            outbound.reject(id);
        }
    }

    log_info!("Zenoh: every outbound route has closed");
    // The subscribers keep delivering for the life of the database.
    core::future::pending::<()>().await;
}

/// Open a session, retrying with a back-off while no router answers.
pub(crate) async fn open(config: zenoh::Config) -> Session {
    let mut wait = OPEN_RETRY;
    let session = loop {
        match zenoh::open(config.clone()).await {
            Ok(session) => break session,
            Err(_e) => {
                log_warn!(
                    "Zenoh: cannot open the session, retrying in {:?}: {}",
                    wait,
                    _e
                );
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(OPEN_RETRY_MAX);
            }
        }
    };
    log_info!("Zenoh: session {} open", session.zid());
    session
}

/// A key written by a topic writer, if it is a valid key without wildcards.
fn written_key(topic: &str) -> Option<&keyexpr> {
    keyexpr::new(topic).ok().filter(|_| !topic.contains('*'))
}

/// One subscriber per filter. A sample matching several filters reaches
/// each subscriber; only the first one dispatches it, so every route sees it
/// once. A sample on a wildcard key is dropped (`ZenohGrammar`), and so is a
/// delete: a record has no value to remove.
async fn subscribe(
    session: &Session,
    inbound: &InboundDispatch,
    filters: Vec<(KeyExpr<'static>, Box<dyn TopicFilter>)>,
) -> Vec<zenoh::pubsub::Subscriber<()>> {
    let warned = Arc::new(AtomicBool::new(false));
    // The filters of the subscribers declared so far: a later subscriber
    // defers to these only, so a failed declaration suppresses nothing.
    let mut earlier: Vec<Arc<dyn TopicFilter>> = Vec::with_capacity(filters.len());
    let mut subscribers = Vec::with_capacity(filters.len());
    for (key, compiled) in filters {
        let inbound = inbound.clone();
        let shadows: Arc<[Arc<dyn TopicFilter>]> = earlier.clone().into();
        let warned = warned.clone();
        let declared = session
            .declare_subscriber(key)
            .allowed_origin(Locality::Remote)
            .callback(move |sample| {
                let key = sample.key_expr().as_str();
                if sample.kind() == SampleKind::Delete {
                    log_debug!("Zenoh: ignored a delete on '{}'", key);
                    return;
                }
                if key.contains('*') {
                    if !warned.swap(true, Ordering::Relaxed) {
                        log_warn!("Zenoh: dropping samples on wildcard keys, first '{}'", key);
                    }
                    log_debug!("Zenoh: dropped a sample on wildcard key '{}'", key);
                    return;
                }
                let mut spans = [(0, 0); MAX_CAPTURES];
                if shadows.iter().any(|f| f.matches(key, &mut spans)) {
                    return;
                }
                inbound.dispatch(key, &sample.payload().to_bytes());
            })
            .await;
        match declared {
            Ok(subscriber) => {
                subscribers.push(subscriber);
                earlier.push(Arc::from(compiled));
            }
            Err(_e) => log_error!("Zenoh: cannot declare a subscriber: {}", _e),
        }
    }
    subscribers
}
