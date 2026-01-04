use aimdb_core::{buffer::BufferCfg, AimDbBuilder, RecordKey};
use aimdb_data_contracts::Linkable;
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use weather_mesh_common::{Humidity, NodeKey, Temperature};

#[tokio::main]
async fn main() -> aimdb_core::DbResult<()> {
    let runtime = std::sync::Arc::new(TokioAdapter::new()?);

    let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
        aimdb_mqtt_connector::MqttConnector::new("mqtt://localhost:1883")
            .with_client_id("tokio-demo-multi-sensor"),
    );

    // Configure Temperature records for all nodes
    for node in [NodeKey::Alpha, NodeKey::Beta, NodeKey::Gamma] {
        let topic = format!("{}temperature", node.link_address().unwrap());
        builder.configure::<Temperature>(node, |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_from(&topic)
                .with_deserializer(Temperature::from_bytes)
                .finish();
        });
    }

    // Configure Humidity records for all nodes
    for node in [NodeKey::Alpha, NodeKey::Beta, NodeKey::Gamma] {
        let topic = format!("{}humidity", node.link_address().unwrap());
        builder.configure::<Humidity>(node, |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_from(&topic)
                .with_deserializer(Humidity::from_bytes)
                .finish();
        });
    }

    builder.run().await
}
