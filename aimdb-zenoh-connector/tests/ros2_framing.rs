//! A `ros2://` link carries bare CDR of the topic's one type: `build()`
//! refuses an inline envelope or another contract version (062).

#![cfg(feature = "std")]

use std::sync::Arc;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, EnvelopeCarrier, RecordRegistrar};
use aimdb_data_contracts::link_codecs::Cdr;
use aimdb_data_contracts::{
    migration_chain, ContractShape, EnvelopeLinkExt, LinkCodecBuilderExt, Linkable, MigrationError,
    MigrationStep, RosMessage, SchemaType, VersionedOutboundExt, WireFormat,
};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_zenoh_connector::{Ros2Connector, Ros2Node};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MoveV1 {
    speed: f32,
}
impl ContractShape for MoveV1 {}
impl SchemaType for MoveV1 {
    const NAME: &'static str = "test_msgs/msg/Move";
    const VERSION: u32 = 1;
}

/// The current version, and the type the ROS topic is declared with.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Move {
    speed: f64,
}
impl ContractShape for Move {}
impl SchemaType for Move {
    const NAME: &'static str = "test_msgs/msg/Move";
    const VERSION: u32 = 2;
}
impl Linkable for Move {
    const WIRE_FORMAT: WireFormat = WireFormat::Cdr;
    fn from_bytes(_: &[u8]) -> Result<Self, String> {
        Ok(Move { speed: 0.0 })
    }
    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
}
impl RosMessage for Move {
    const ROS_TYPE_NAME: &'static str = "test_msgs::msg::dds_::Move_";
    const ROS_TYPE_HASH: &'static str =
        "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
}

struct MoveV1ToV2;
impl MigrationStep for MoveV1ToV2 {
    type Older = MoveV1;
    type Newer = Move;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;
    fn up(v1: MoveV1) -> Result<Move, MigrationError> {
        Ok(Move {
            speed: f64::from(v1.speed),
        })
    }
    fn down(v2: Move) -> Result<MoveV1, MigrationError> {
        Ok(MoveV1 {
            speed: v2.speed as f32,
        })
    }
}

migration_chain! {
    type Current = Move;
    steps {
        MoveV1ToV2: MoveV1 => Move,
    }
}

async fn build(
    link: impl FnOnce(&mut RecordRegistrar<'_, Move>) + Send + 'static,
) -> Result<(), String> {
    let key_move_1 = aimdb_core::Name::<Move>::new("move");
    let connector = Ros2Connector::new("", Ros2Node::new("aimdb_test").namespace("/cell4"))
        .domain_id(0)
        .register::<Move>();
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter::new().unwrap()))
        .with_connector(connector);
    builder.configure(key_move_1, move |reg| {
        reg.buffer(BufferCfg::SingleLatest);
        link(reg);
    });
    builder.build().await.map(|_| ()).map_err(|e| e.to_string())
}

#[tokio::test]
async fn build_refuses_an_inline_envelope() {
    let e = build(|reg| {
        reg.link_to("ros2://cell4/move")
            .with_link_codec(Cdr::<64>)
            .with_envelope(EnvelopeCarrier::Inline)
            .finish();
    })
    .await
    .unwrap_err();
    assert!(e.contains("inline envelope"), "outbound: {e}");

    let e = build(|reg| {
        reg.link_from("ros2://cell4/move")
            .with_link_codec(Cdr::<64>)
            .with_envelope(EnvelopeCarrier::Inline)
            .finish();
    })
    .await
    .unwrap_err();
    assert!(e.contains("inline envelope"), "inbound: {e}");
}

#[tokio::test]
async fn build_refuses_another_contract_version() {
    let e = build(|reg| {
        reg.link_to("ros2://cell4/move")
            .with_link_codec(Cdr::<64>)
            .sends_as::<1>()
            .finish();
    })
    .await
    .unwrap_err();
    assert!(e.contains("contract version 1"), "sends_as: {e}");

    let e = build(|reg| {
        reg.link_from("ros2://cell4/move")
            .with_link_codec(Cdr::<64>)
            .with_contract::<MoveV1>()
            .finish();
    })
    .await
    .unwrap_err();
    assert!(e.contains("contract version 1"), "with_contract: {e}");
}

/// The framing check passes a bare link bound to the current version.
#[tokio::test]
async fn the_current_version_is_not_refused_for_its_framing() {
    let result = build(|reg| {
        reg.link_to("ros2://cell4/move")
            .with_link_codec(Cdr::<64>)
            .with_contract::<Move>()
            .finish();
    })
    .await;
    if let Err(e) = result {
        assert!(
            !e.contains("envelope") && !e.contains("contract version"),
            "{e}"
        );
    }
}
