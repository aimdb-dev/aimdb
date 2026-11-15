//! An AimX client carries record payloads as JSON inside its frames, so a
//! link with an inline envelope (a binary header) must fail `build()`: sent
//! anyway, it fails to encode and the client drops the session every link
//! shares.

use std::sync::Arc;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, EnvelopeCarrier, RecordRegistrar};
use aimdb_data_contracts::link_codecs::Json;
use aimdb_data_contracts::{
    migration_chain, ContractShape, EnvelopeLinkExt, LinkCodecBuilderExt, SchemaType,
};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use aimdb_uds_connector::UdsClient;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Reading {
    celsius: f32,
}
impl ContractShape for Reading {}
impl SchemaType for Reading {
    const NAME: &'static str = "reading";
}
migration_chain! { type Current = Reading; steps {} }

/// Builds a client db with one `reading` record linked by `link`. Nothing is
/// dialed: `build()` only assembles the connector's futures.
async fn build(
    link: impl FnOnce(&mut RecordRegistrar<'_, Reading>) + Send + 'static,
) -> Result<(), String> {
    let key_reading_1 = aimdb_core::Name::<Reading>::new("reading");
    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(UdsClient::new("/nonexistent/aimdb-test.sock"));
    builder.configure(key_reading_1, move |reg| {
        reg.buffer(BufferCfg::SingleLatest);
        link(reg);
    });
    builder.build().await.map(|_| ()).map_err(|e| e.to_string())
}

#[tokio::test]
async fn build_refuses_an_inline_envelope() {
    let e = build(|reg| {
        reg.link_to("uds://reading")
            .with_link_codec(Json)
            .with_envelope(EnvelopeCarrier::Inline)
            .finish();
    })
    .await
    .unwrap_err();
    assert!(
        e.contains("uds://reading") && e.contains("inline envelope"),
        "outbound: {e}"
    );

    let e = build(|reg| {
        reg.link_from("uds://reading")
            .with_link_codec(Json)
            .with_envelope(EnvelopeCarrier::Inline)
            .finish();
    })
    .await
    .unwrap_err();
    assert!(
        e.contains("uds://reading") && e.contains("inline envelope"),
        "inbound: {e}"
    );
}

#[tokio::test]
async fn a_bare_link_still_builds() {
    build(|reg| {
        reg.link_to("uds://reading")
            .with_link_codec(Json)
            .with_envelope(EnvelopeCarrier::Bound)
            .finish();
    })
    .await
    .expect("a bound (bare) link builds");
}
