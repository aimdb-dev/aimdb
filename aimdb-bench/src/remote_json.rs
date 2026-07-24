//! Production-path harness for issue #196's direct JSON-byte optimization.
//!
//! This intentionally stops at the in-memory AimX envelope boundary. It runs
//! the real `TypedRecord` codec, runtime buffer, type-erased reader, `Payload`,
//! and `AimxCodec`; it does not include a socket, scheduler wakeup, or physical
//! transport.

use std::sync::Arc;

use aimdb_core::buffer::{BufferCfg, JsonBufferReader};
use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::{
    AimDb, AimDbBuilder, EnvelopeCodec, Outbound, Payload, Producer, RecordRegistrar,
};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
use serde::{Deserialize, Serialize};

const RECORD_KEY: &str = "bench.remote-json";

/// The two record serialization paths compared by the focused benchmarks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteJsonPath {
    /// Compatibility path: `T -> serde_json::Value -> JSON bytes`.
    Tree,
    /// Issue #196 path: `T -> JSON bytes`.
    Direct,
}

impl RemoteJsonPath {
    /// Stable label used by benchmark output.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tree => "tree",
            Self::Direct => "direct",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Coordinates {
    latitude_e6: i32,
    longitude_e6: i32,
}

/// Heap-free typed fixture so measured allocations belong to JSON/session
/// representation work rather than cloning the record itself.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct RemoteJsonSample {
    sequence: u64,
    temperature_c: f32,
    quality: Option<u16>,
    flags: [bool; 4],
    coordinates: Coordinates,
}

fn sample(sequence: u64) -> RemoteJsonSample {
    RemoteJsonSample {
        sequence,
        temperature_c: 23.75,
        quality: Some(97),
        flags: [true, false, true, true],
        coordinates: Coordinates {
            latitude_e6: 10_776_900,
            longitude_e6: 106_700_900,
        },
    }
}

/// Reusable production-path state for allocation and latency measurements.
pub struct RemoteJsonHarness {
    db: AimDb,
    producer: Producer<RemoteJsonSample>,
    reader: Box<dyn JsonBufferReader + Send>,
    sequence: u64,
    frame: Vec<u8>,
}

impl RemoteJsonHarness {
    /// Build one database with a remote-accessible `SingleLatest` record.
    ///
    /// The returned runner is intentionally not started: all measured
    /// operations are synchronous buffer/codec/session work and no services
    /// are registered.
    pub async fn new() -> Self {
        let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
        builder.configure::<RemoteJsonSample>(RECORD_KEY, |record: &mut RecordRegistrar<_>| {
            record.buffer(BufferCfg::SingleLatest).with_remote_access();
        });

        let (db, _runner) = builder.build().await.expect("remote JSON bench db");
        let producer = db
            .producer::<RemoteJsonSample>(RECORD_KEY)
            .expect("remote JSON bench producer");
        producer.produce(sample(0));

        let reader = {
            let id = db
                .inner()
                .resolve_str(RECORD_KEY)
                .expect("remote JSON bench record id");
            db.inner()
                .storage(id)
                .expect("remote JSON bench record")
                .json_access()
                .expect("remote JSON access")
                .subscribe_json()
                .expect("remote JSON reader")
        };

        Self {
            db,
            producer,
            reader,
            sequence: 0,
            frame: Vec::with_capacity(512),
        }
    }

    /// Encode canonical latest state into a production AimX reply frame.
    pub fn record_get_frame(&mut self, path: RemoteJsonPath) -> &[u8] {
        let payload = match path {
            RemoteJsonPath::Tree => {
                let value = self
                    .db
                    .try_latest_as_json(RECORD_KEY)
                    .expect("bench latest JSON tree");
                Payload::from(serde_json::to_vec(&value).expect("bench tree serialization"))
            }
            RemoteJsonPath::Direct => Payload::from(
                self.db
                    .try_latest_as_json_bytes(RECORD_KEY)
                    .expect("bench latest JSON bytes"),
            ),
        };

        self.frame.clear();
        AimxCodec
            .encode(
                Outbound::Reply {
                    id: 1,
                    result: Ok(payload),
                },
                &mut self.frame,
            )
            .expect("bench AimX reply");
        &self.frame
    }

    /// Push one typed value, consume it through the type-erased subscription,
    /// and encode a production AimX event frame.
    pub fn subscription_event_frame(&mut self, path: RemoteJsonPath) -> &[u8] {
        self.sequence = self.sequence.wrapping_add(1);
        self.producer.produce(sample(self.sequence));

        let payload = match path {
            RemoteJsonPath::Tree => {
                let value = self
                    .reader
                    .try_recv_json()
                    .expect("bench subscription JSON tree");
                Payload::from(serde_json::to_vec(&value).expect("bench tree serialization"))
            }
            RemoteJsonPath::Direct => Payload::from(
                self.reader
                    .try_recv_json_bytes()
                    .expect("bench subscription JSON bytes"),
            ),
        };

        self.frame.clear();
        AimxCodec
            .encode(
                Outbound::Event {
                    sub: "1",
                    seq: self.sequence,
                    topic: None,
                    data: payload,
                },
                &mut self.frame,
            )
            .expect("bench AimX event");
        &self.frame
    }
}

/// Assert that both paths produce equivalent JSON/AimX semantics.
pub async fn verify_remote_json_paths() {
    let mut tree = RemoteJsonHarness::new().await;
    let mut direct = RemoteJsonHarness::new().await;

    let tree_get: serde_json::Value =
        serde_json::from_slice(tree.record_get_frame(RemoteJsonPath::Tree))
            .expect("tree get frame");
    let direct_get: serde_json::Value =
        serde_json::from_slice(direct.record_get_frame(RemoteJsonPath::Direct))
            .expect("direct get frame");
    assert_eq!(tree_get, direct_get);

    let tree_event: serde_json::Value =
        serde_json::from_slice(tree.subscription_event_frame(RemoteJsonPath::Tree))
            .expect("tree event frame");
    let direct_event: serde_json::Value =
        serde_json::from_slice(direct.subscription_event_frame(RemoteJsonPath::Direct))
            .expect("direct event frame");
    assert_eq!(tree_event, direct_event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_and_tree_paths_have_equivalent_aimx_json() {
        verify_remote_json_paths().await;
    }

    #[test]
    fn path_names_are_stable() {
        assert_eq!(RemoteJsonPath::Tree.name(), "tree");
        assert_eq!(RemoteJsonPath::Direct.name(), "direct");
    }
}
