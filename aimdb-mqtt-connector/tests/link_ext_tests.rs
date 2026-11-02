//! Tests for the MQTT link extension traits (`MqttLinkExt` / `MqttOutboundLinkExt`)
//!
//! `with_qos`/`with_retain` live outside core's generic link builders into this crate. These tests pin down the wire-compat contract:
//! the extension methods push exactly the `("qos", …)` / `("retain", …)`
//! option keys the MQTT clients read from `protocol_options`.

#![cfg(feature = "std")]

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, InboundDispatch, InboundLink, OutboundLink, OutboundRoutes};
use aimdb_data_contracts::{
    link_codecs, publish, subscribe, LinkCodec, LinkCodecBuilderExt, Linkable, SchemaType,
};
use aimdb_mqtt_connector::{MqttConnector, MqttLinkExt, MqttOutboundLinkExt};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Reading {
    value: f32,
}

impl SchemaType for Reading {
    const NAME: &'static str = "test.reading";
}

impl Linkable for Reading {
    fn from_bytes(data: &[u8]) -> Result<Self, String> {
        let bytes = data.try_into().map_err(|_| "need 4 bytes".to_string())?;
        Ok(Reading {
            value: f32::from_le_bytes(bytes),
        })
    }

    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        Ok(self.value.to_le_bytes().to_vec())
    }
}

#[tokio::test]
async fn outbound_knobs_push_exact_config_keys() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(MqttConnector::new("mqtt://localhost:1883").with_client_id("link-ext"));

    builder.configure::<Reading>("test.reading.out", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_to("mqtt://sensors/reading")
            .with_qos(2)
            .with_retain(true)
            .with_serializer(|_ctx, _r: &Reading| Ok(vec![0u8]))
            .finish();
    });

    let (db, _runner) = builder.build().await.expect("build must succeed");

    let id = db.inner().resolve_str("test.reading.out").unwrap();
    let record = db.inner().storage(id).unwrap();
    let config = &record.outbound_connectors()[0].config;

    assert!(
        config.contains(&("qos".to_string(), "2".to_string())),
        "expected (qos, 2) in {config:?}"
    );
    assert!(
        config.contains(&("retain".to_string(), "true".to_string())),
        "expected (retain, true) in {config:?}"
    );
}

#[tokio::test]
async fn inbound_qos_pushes_exact_config_key() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new()
        .runtime(runtime)
        .with_connector(MqttConnector::new("mqtt://localhost:1883").with_client_id("link-ext-in"));

    builder.configure::<Reading>("test.reading.in", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from("mqtt://commands/reading")
            .with_qos(0)
            .with_deserializer(|_ctx, _b: &[u8]| Ok(Reading { value: 0.0 }))
            .finish();
    });

    let (db, _runner) = builder.build().await.expect("build must succeed");

    let id = db.inner().resolve_str("test.reading.in").unwrap();
    let record = db.inner().storage(id).unwrap();
    let config = &record.inbound_connectors()[0].config;

    assert!(
        config.contains(&("qos".to_string(), "0".to_string())),
        "expected (qos, 0) in {config:?}"
    );
}

#[tokio::test]
async fn per_link_codec_preserves_mqtt_extensions_and_wiring() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        MqttConnector::new("mqtt://localhost:1883").with_client_id("link-ext-codec"),
    );

    builder.configure::<Reading>("test.reading.codec", |reg| {
        reg.buffer(BufferCfg::SingleLatest);

        reg.link_to("mqtt://sensors/codec")
            .with_link_codec(link_codecs::Postcard::<64>)
            .with_qos(2)
            .with_retain(true)
            .finish();

        reg.link_from("mqtt://commands/codec")
            .with_link_codec(link_codecs::Postcard::<64>)
            .with_qos(0)
            .finish();
    });

    let (db, _runner) = builder.build().await.expect("build must succeed");

    let outbound = OutboundRoutes::new(&db, "mqtt").expect("outbound routes");
    let routes = outbound.routes();
    assert_eq!(routes.len(), 1);
    assert_eq!(&*routes[0].default_topic, "sensors/codec");
    assert_eq!(routes[0].payload_capacity, 64);
    let options = &routes[0].config.protocol_options;
    assert!(options.contains(&("qos".to_string(), "2".to_string())));
    assert!(options.contains(&("retain".to_string(), "true".to_string())));

    let id = db.inner().resolve_str("test.reading.codec").unwrap();
    let record = db.inner().storage(id).unwrap();
    let inbound_config = &record.inbound_connectors()[0].config;
    assert!(inbound_config.contains(&("qos".to_string(), "0".to_string())));

    let inbound =
        InboundDispatch::new(&db, "mqtt", &aimdb_core::ExactGrammar).expect("inbound routes");
    assert_eq!(inbound.subscriptions(), [Arc::from("commands/codec")]);
    let encoded = link_codecs::Postcard::<64>
        .encode(&Reading { value: 17.5 })
        .expect("Postcard encode must succeed");
    let mut reader = db
        .subscribe::<Reading>("test.reading.codec")
        .expect("subscribe");
    inbound.dispatch("commands/codec", &encoded);
    assert_eq!(
        reader.try_recv().expect("Postcard ingest").value.value,
        17.5
    );
}

/// The knobs and the codec verb work on detached links too, and push the
/// same config as on `link_to` / `link_from`.
#[tokio::test]
async fn detached_links_take_the_same_knobs() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        MqttConnector::new("mqtt://localhost:1883").with_client_id("link-ext-detached"),
    );

    builder.configure::<Reading>("test.reading.attached", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
        reg.link_to("mqtt://sensors/attached")
            .with_link_codec(link_codecs::Postcard::<64>)
            .with_qos(2)
            .with_retain(true)
            .finish();
        reg.link_from("mqtt://commands/attached")
            .with_link_codec(link_codecs::Postcard::<64>)
            .with_qos(0)
            .finish();
    });
    builder.configure::<Reading>("test.reading.detached", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .output(
                OutboundLink::new("mqtt://sensors/detached")
                    .with_link_codec(link_codecs::Postcard::<64>)
                    .with_qos(2)
                    .with_retain(true),
            )
            .input(
                InboundLink::new("mqtt://commands/detached")
                    .with_link_codec(link_codecs::Postcard::<64>)
                    .with_qos(0),
            );
    });

    let (db, _runner) = builder.build().await.expect("build must succeed");

    let record = |key: &str| {
        let id = db.inner().resolve_str(key).unwrap();
        db.inner().storage(id).unwrap()
    };
    let (attached, detached) = (
        record("test.reading.attached"),
        record("test.reading.detached"),
    );
    assert_eq!(
        attached.outbound_connectors()[0].config,
        detached.outbound_connectors()[0].config
    );
    assert_eq!(
        attached.inbound_connectors()[0].config,
        detached.inbound_connectors()[0].config
    );
    let out = &detached.outbound_connectors()[0].config;
    assert!(out.contains(&("qos".to_string(), "2".to_string())));
    assert!(out.contains(&("retain".to_string(), "true".to_string())));

    let outbound = OutboundRoutes::new(&db, "mqtt").expect("outbound routes");
    let payload_capacities: Vec<usize> = outbound
        .routes()
        .iter()
        .map(|r| r.payload_capacity)
        .collect();
    assert_eq!(payload_capacities, [64, 64], "both take the bounded path");
}

/// `publish` / `subscribe` take the MQTT knobs like any other link.
#[tokio::test]
async fn publish_and_subscribe_take_the_knobs() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);
    let mqtt = builder
        .connect(MqttConnector::new("mqtt://localhost:1883").with_client_id("link-ext-publish"));

    builder.configure::<Reading>("test.reading.contract", move |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .output(
                publish(mqtt, "sensors/contract")
                    .with_qos(2)
                    .with_retain(true),
            )
            .input(subscribe(mqtt, "commands/contract").with_qos(0));
    });

    let (db, _runner) = builder.build().await.expect("build must succeed");

    let id = db.inner().resolve_str("test.reading.contract").unwrap();
    let record = db.inner().storage(id).unwrap();
    let out = &record.outbound_connectors()[0];
    assert_eq!(out.url.to_string(), "mqtt://sensors/contract");
    assert!(out.config.contains(&("qos".to_string(), "2".to_string())));
    assert!(out
        .config
        .contains(&("retain".to_string(), "true".to_string())));
    let inbound = &record.inbound_connectors()[0];
    assert_eq!(inbound.url.to_string(), "mqtt://commands/contract");
    assert!(inbound
        .config
        .contains(&("qos".to_string(), "0".to_string())));
}
