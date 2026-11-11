//! Buffer metrics look up records by their configured keys.

use std::sync::Arc;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::remote::AimxConfig;
use aimdb_core::AimDbBuilder;
use aimdb_mcp::{connection::ConnectionPool, tools};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::UdsServer;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Reading {
    value: u64,
}

#[tokio::test]
async fn get_buffer_metrics_finds_the_configured_record_key() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("aimdb.sock");
    let endpoint = sock.to_str().unwrap();
    let config = AimxConfig::uds_default().socket_path(endpoint);
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsServer::from_config(config));
    for key in ["sensor.indoor", "sensor.outdoor"] {
        builder.configure::<Reading>(key, |reg| {
            reg.buffer(BufferCfg::SingleLatest).with_remote_access();
        });
    }
    let (_db, runner) = builder.build().await.expect("build db");
    tokio::spawn(runner.run());
    tools::init_connection_pool(ConnectionPool::new());

    let result = tools::get_buffer_metrics(Some(json!({
        "endpoint": endpoint,
        "record_key": "sensor.indoor",
    })))
    .await
    .expect("get buffer metrics");
    assert_eq!(result["found"], true);
    let records = result["records"].as_array().expect("matching records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["record_key"], "sensor.indoor");
}
