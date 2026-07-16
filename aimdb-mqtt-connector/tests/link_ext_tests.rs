//! Tests for the MQTT link extension traits (`MqttLinkExt` / `MqttOutboundLinkExt`)
//!
//! `with_qos`/`with_retain` live outside core's generic link builders into this crate. These tests pin down the wire-compat contract:
//! the extension methods push exactly the `("qos", …)` / `("retain", …)`
//! option keys the MQTT clients read from `protocol_options`.

#![cfg(feature = "tokio-runtime")]

use aimdb_core::buffer::BufferCfg;
use aimdb_core::AimDbBuilder;
use aimdb_data_contracts::{link_codecs, LinkCodec, LinkCodecBuilderExt};
use aimdb_mqtt_connector::{MqttConnector, MqttLinkExt, MqttOutboundLinkExt};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Reading {
    #[allow(dead_code)]
    value: f32,
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

    let outbound = db.collect_outbound_routes("mqtt");
    assert_eq!(outbound.len(), 1);
    assert_eq!(outbound[0].topic, "sensors/codec");
    assert_eq!(outbound[0].source.serializer_scratch_capacity(), Some(64));
    assert!(outbound[0]
        .config
        .contains(&("qos".to_string(), "2".to_string())));
    assert!(outbound[0]
        .config
        .contains(&("retain".to_string(), "true".to_string())));

    let id = db.inner().resolve_str("test.reading.codec").unwrap();
    let record = db.inner().storage(id).unwrap();
    let inbound_config = &record.inbound_connectors()[0].config;
    assert!(inbound_config.contains(&("qos".to_string(), "0".to_string())));

    let inbound = db.collect_inbound_routes("mqtt");
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0].0, "commands/codec");
    let encoded = link_codecs::Postcard::<64>
        .encode(&Reading { value: 17.5 })
        .expect("Postcard encode must succeed");
    inbound[0].1(&db.runtime_ctx(), &encoded).expect("Postcard ingest must succeed");
}
