//! End-to-end registration of inline and statically bound link codec closures.
#![cfg(all(feature = "migratable", feature = "linkable-postcard"))]
#[path = "fixtures/admission_contracts.rs"]
mod contracts;
use aimdb_core::buffer::{BufferReader, DynBuffer};
use aimdb_core::connector::{ConnectorBuilder, SerializeError};
use aimdb_core::envelope::{ContractRef, Envelope, HeaderWriter, PayloadCodec};
use aimdb_core::executor::test_support::NoopRuntimeOps;
use aimdb_core::{
    AimDb, AimDbBuilder, BoxFuture, DbError, DbResult, ExactGrammar, InboundDispatch, InboundLink,
    OutboundLink, OutboundPayload, OutboundRoutes, RecordRegistrar, Stamp, Stamped, WriteMetadata,
};
use aimdb_data_contracts::{
    link_codecs::Postcard, EnvelopeCarrier, EnvelopeLinkExt, LinkCodec, LinkCodecBuilderExt,
    MigrationChain, PayloadDecoder, PayloadEncoder, VersionedCodecBuilderExt, VersionedInboundExt,
    VersionedOutboundExt,
};
use contracts::{V1, V2};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use std::sync::{Arc, Mutex};

struct Noop(&'static str);
impl ConnectorBuilder for Noop {
    fn build<'a>(
        &'a self,
        _: &'a AimDb,
    ) -> Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn scheme(&self) -> &str {
        self.0
    }
}
type Capture<T> = Arc<Mutex<Vec<Stamped<T>>>>;
struct Buffer<T> {
    value: T,
    captured: Capture<T>,
}
struct Reader<T> {
    value: Option<T>,
}
impl<T: Clone + Send + Sync + 'static> DynBuffer<Stamped<T>> for Buffer<T> {
    fn push(&self, value: Stamped<T>) {
        self.captured.lock().unwrap().push(value);
    }
    fn subscribe_boxed(&self) -> Box<dyn BufferReader<Stamped<T>> + Send> {
        Box::new(Reader {
            value: Some(Stamped::new(
                self.value.clone(),
                WriteMetadata::from_wire_stamp(Stamp::from_nanos(0)),
            )),
        })
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
impl<T: Clone + Send> BufferReader<T> for Reader<T> {
    fn poll_recv(&mut self, _: &mut Context<'_>) -> Poll<Result<T, DbError>> {
        Poll::Ready(self.try_recv())
    }
    fn try_recv(&mut self) -> Result<T, DbError> {
        self.value.take().ok_or_else(|| DbError::BufferClosed {
            buffer_name: "versioned-link-test".into(),
        })
    }
}
fn builder() -> AimDbBuilder {
    AimDbBuilder::new()
        .runtime(Arc::new(NoopRuntimeOps))
        .with_connector(Noop("test"))
}
fn value() -> V2 {
    V2 {
        celsius: 21.5,
        quality: 300,
    }
}
async fn db(
    configure: impl FnOnce(&mut RecordRegistrar<'_, V2, aimdb_core::Declared>),
) -> (AimDb, Capture<V2>) {
    let key_reading_1 = aimdb_data_contracts::Key::<V2>::new("reading");
    let captured: Capture<V2> = Default::default();
    let mut b = builder();
    b.configure(key_reading_1, |r| {
        r.buffer_raw(Box::new(Buffer {
            value: value(),
            captured: captured.clone(),
        }));
        configure(r);
    });
    (b.build().await.unwrap().0, captured)
}
fn frame(version: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = [0; 19];
    let n = HeaderWriter::new(&mut out)
        .contract(ContractRef::new(
            aimdb_data_contracts::name_id("temperature"),
            version,
            V2::shape_of(version),
        ))
        .codec(PayloadCodec::Postcard)
        .finish()
        .unwrap();
    let mut bytes = out[..n].to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn inbound_options_work_in_either_order_and_on_detached_links() {
    futures::executor::block_on(async {
        let (db, captured) = db(|r| {
            r.link_from("test://inline")
                .accepts_from::<1>()
                .with_link_codec(Postcard::<32>)
                .with_envelope(EnvelopeCarrier::Inline)
                .finish();
            r.input(
                InboundLink::new("test://bound")
                    .with_contract::<V1>()
                    .with_link_codec(Postcard::<32>),
            );
            r.input(
                InboundLink::new("test://current")
                    .with_link_codec(Postcard::<32>)
                    .with_envelope(EnvelopeCarrier::Inline),
            );
            r.link_from("test://default")
                .with_link_codec(Postcard::<32>)
                .finish();
        })
        .await;
        let inbound = InboundDispatch::new(&db, "test", &ExactGrammar).unwrap();
        let old = 21.5_f32.to_le_bytes();
        inbound.dispatch("inline", &frame(1, &old));
        inbound.dispatch("bound", &old);
        assert_eq!(
            captured
                .lock()
                .unwrap()
                .iter()
                .map(|v| v.value.quality)
                .collect::<Vec<_>>(),
            [1, 1]
        );
        assert_eq!(inbound.stats(0).unwrap().upgraded[0].count, 1);
        assert_eq!(inbound.stats(1).unwrap().upgraded[0].count, 1);
        inbound.dispatch("current", &frame(1, &old));
        assert_eq!(inbound.stats(2).unwrap().too_old, 1);
        let payload = Postcard::<32>.encode(&value()).unwrap();
        inbound.dispatch("default", &payload);
        assert_eq!(captured.lock().unwrap().last().unwrap().value, value());
        let count = captured.lock().unwrap().len();
        inbound.dispatch("default", &frame(2, &payload));
        assert_eq!(inbound.stats(3).unwrap().decode_error, 1);
        assert_eq!(captured.lock().unwrap().len(), count);
        let schema = db
            .inner()
            .dependency_graph()
            .node("reading")
            .unwrap()
            .schema
            .clone()
            .unwrap();
        assert_eq!((schema.name.as_str(), schema.version), ("temperature", 2));
    });
}

#[test]
fn outbound_headers_downgrades_shapes_and_owned_fallback() {
    futures::executor::block_on(async {
        let (db, _) = db(|r| {
            r.link_to("test://current")
                .with_link_codec(Postcard::<32>)
                .with_envelope(EnvelopeCarrier::Inline)
                .finish();
            r.output(
                OutboundLink::new("test://old")
                    .sends_as::<1>()
                    .with_envelope(EnvelopeCarrier::Inline)
                    .with_link_codec(Postcard::<32>),
            );
            r.link_to("test://bare")
                .with_contract::<V1>()
                .sends_as::<1>()
                .with_link_codec(Postcard::<32>)
                .finish();
            r.link_to("test://no-shape")
                .without_shape()
                .with_link_codec(Postcard::<32>)
                .with_envelope(EnvelopeCarrier::Inline)
                .finish();
            r.link_to("test://fallback")
                .with_link_codec(Postcard::<1>)
                .with_envelope(EnvelopeCarrier::Inline)
                .finish();
        })
        .await;
        let mut routes = OutboundRoutes::new(&db, "test").unwrap();
        for _ in 0..5 {
            let msg = routes.next().await.unwrap();
            if msg.topic == "bare" {
                assert_eq!(msg.payload.as_slice(), 21.5_f32.to_le_bytes());
                assert_eq!(msg.route.payload_capacity, 32);
                continue;
            }
            let env = Envelope::parse(msg.payload.as_slice()).unwrap();
            let old = msg.topic == "old";
            assert_eq!(env.contract().version(), if old { 1 } else { 2 });
            assert_eq!(env.codec(), PayloadCodec::Postcard);
            assert_eq!(
                env.contract().shape(),
                if msg.topic == "no-shape" {
                    None
                } else {
                    V2::shape_of(if old { 1 } else { 2 })
                }
            );
            let expected = if old {
                21.5_f32.to_le_bytes().to_vec()
            } else {
                Postcard::<32>.encode(&value()).unwrap()
            };
            assert_eq!(env.payload(), expected);
            if msg.topic == "fallback" {
                assert!(matches!(msg.payload, OutboundPayload::Owned(_)));
                assert_eq!(msg.route.payload_capacity, 20);
            } else {
                assert_eq!(msg.route.payload_capacity, 51);
            }
        }
    });
}

fn invalid(configure: impl FnOnce(&mut RecordRegistrar<'_, V2, aimdb_core::Declared>)) -> String {
    futures::executor::block_on(async {
        let key_reading_2 = aimdb_data_contracts::Key::<V2>::new("reading");
        let mut b = builder();
        b.configure(key_reading_2, |r| {
            r.buffer_raw(Box::new(Buffer {
                value: value(),
                captured: Default::default(),
            }));
            configure(r);
        });
        let Err(error) = b.build().await else {
            panic!("invalid declarations compiled")
        };
        error.to_string()
    })
}
#[test]
fn conflicting_declarations_fail_registration() {
    assert!(invalid(|r| {
        r.link_to("test://out")
            .with_link_codec(Postcard::<{ usize::MAX }>)
            .with_envelope(EnvelopeCarrier::Inline)
            .finish();
    })
    .contains("scratch capacity overflow"));

    assert!(invalid(|r| {
        r.link_from("test://in")
            .with_link_codec(Postcard::<32>)
            .accepts_from::<1>()
            .finish();
    })
    .contains("accepts_from requires"));
    assert!(invalid(|r| {
        r.link_from("test://in")
            .with_link_codec(Postcard::<32>)
            .with_contract::<V1>()
            .with_envelope(EnvelopeCarrier::Inline)
            .finish();
    })
    .contains("with_contract is only valid"));
    assert!(invalid(|r| {
        r.link_to("test://out")
            .with_link_codec(Postcard::<32>)
            .with_contract::<V1>()
            .sends_as::<2>()
            .finish();
    })
    .contains("different versions"));
    assert!(invalid(|r| {
        r.link_to("test://out")
            .with_link_codec(Postcard::<32>)
            .with_envelope(EnvelopeCarrier::Inline)
            .with_serializer(|_, _| Ok(Vec::new()))
            .finish();
    })
    .contains("select a builtin codec"));
    assert!(invalid(|r| {
        r.link_from("test://in")
            .with_link_codec(Postcard::<32>)
            .with_config("envelope", "inline")
            .finish();
    })
    .contains("inline envelopes require"));
    assert!(invalid(|r| {
        r.link_from("test://in")
            .with_link_codec(Postcard::<32>)
            .with_envelope(EnvelopeCarrier::Inline)
            .with_config("envelope", "other")
            .finish();
    })
    .contains("envelope must"));
}

#[derive(Clone)]
struct Tagged(u8);
impl PayloadDecoder for Tagged {
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<(T, usize), String> {
        if bytes.first() != Some(&self.0) {
            return Err("wrong custom codec tag".into());
        }
        let (value, used) = Postcard::<32>.decode_payload(&bytes[1..])?;
        Ok((value, used + 1))
    }
}
impl PayloadEncoder for Tagged {
    fn encode_payload<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut [u8],
    ) -> Result<usize, SerializeError> {
        let (tag, rest) = out
            .split_first_mut()
            .ok_or(SerializeError::BufferTooSmall)?;
        *tag = self.0;
        Postcard::<32>.encode_payload(value, rest).map(|n| n + 1)
    }
}
impl<T: serde::Serialize + serde::de::DeserializeOwned> LinkCodec<T> for Tagged {
    const ENCODE_BUFFER_CAPACITY: Option<usize> = Some(32);
    fn decode(&self, bytes: &[u8]) -> Result<T, String> {
        self.decode_payload(bytes).map(|(v, _)| v)
    }
    fn encode(&self, value: &T) -> Result<Vec<u8>, SerializeError> {
        self.encode_payload_owned(value)
    }
    fn encode_into(&self, value: &T, out: &mut [u8]) -> Result<usize, SerializeError> {
        self.encode_payload(value, out)
    }
}
#[test]
fn custom_codec_state_survives_declarations_and_newer_codec_replacement() {
    futures::executor::block_on(async {
        let (db, captured) = db(|r| {
            r.link_from("test://in")
                .with_versioned_codec(Tagged(42))
                .with_envelope(EnvelopeCarrier::Inline)
                .accepts_from::<1>()
                .finish();
            r.link_to("test://out")
                .with_envelope(EnvelopeCarrier::Inline)
                .sends_as::<1>()
                .with_versioned_codec(Tagged(42))
                .finish();
            r.link_to("test://replaced")
                .with_versioned_codec(Tagged(42))
                .with_envelope(EnvelopeCarrier::Inline)
                .with_link_codec(Postcard::<32>)
                .finish();
        })
        .await;
        let mut routes = OutboundRoutes::new(&db, "test").unwrap();
        let inbound = InboundDispatch::new(&db, "test", &ExactGrammar).unwrap();
        for _ in 0..2 {
            let msg = routes.next().await.unwrap();
            let env = Envelope::parse(msg.payload.as_slice()).unwrap();
            if msg.topic == "out" {
                assert_eq!(env.codec(), PayloadCodec::Unspecified);
                assert_eq!(env.payload()[0], 42);
                inbound.dispatch("in", msg.payload.as_slice());
                assert_eq!(captured.lock().unwrap()[0].value.quality, 1);
            } else {
                assert_eq!(env.codec(), PayloadCodec::Postcard);
                assert_eq!(env.payload(), Postcard::<32>.encode(&value()).unwrap());
            }
        }
    });
}

#[cfg(feature = "linkable-json")]
#[test]
fn replacing_bounded_codec_with_json_clears_scratch_and_encodes_inline() {
    futures::executor::block_on(async {
        let (db, _) = db(|r| {
            r.link_to("test://json")
                .with_envelope(EnvelopeCarrier::Inline)
                .with_link_codec(Postcard::<32>)
                .with_link_codec(aimdb_data_contracts::link_codecs::Json)
                .finish();
        })
        .await;
        let mut routes = OutboundRoutes::new(&db, "test").unwrap();
        let msg = routes.next().await.unwrap();
        assert_eq!(msg.route.payload_capacity, 0);
        let env = Envelope::parse(msg.payload.as_slice()).unwrap();
        assert_eq!(env.codec(), PayloadCodec::Json);
        assert_eq!(env.payload(), serde_json::to_vec(&value()).unwrap());
        assert!(matches!(msg.payload, OutboundPayload::Owned(_)));
    });
}

#[cfg(feature = "linkable-cdr")]
#[test]
fn cdr_inline_payload_and_foreign_bare_publication_match_the_codec() {
    futures::executor::block_on(async {
        let key_reading_3 = aimdb_data_contracts::Key::<V2>::new("reading");
        use aimdb_data_contracts::link_codecs::Cdr;
        let mut b = builder().with_connector(Noop("ros2"));
        b.configure(key_reading_3, |r| {
            r.buffer_raw(Box::new(Buffer {
                value: value(),
                captured: Default::default(),
            }));
            r.link_to("test://inline")
                .with_link_codec(Cdr::<32>)
                .with_envelope(EnvelopeCarrier::Inline)
                .finish();
            r.link_to("ros2://foreign")
                .with_link_codec(Cdr::<32>)
                .finish();
        });
        let (db, _) = b.build().await.unwrap();
        let expected = Cdr::<32>.encode(&value()).unwrap();
        let mut inline = OutboundRoutes::new(&db, "test").unwrap();
        let msg = inline.next().await.unwrap();
        assert_eq!(
            Envelope::parse(msg.payload.as_slice()).unwrap().payload(),
            expected
        );
        let mut foreign = OutboundRoutes::new(&db, "ros2").unwrap();
        assert_eq!(foreign.next().await.unwrap().payload.as_slice(), expected);
    });
}

/// Plain `with_link_codec` links (no envelope) follow the one-frame-one-value
/// rule too; CDR padding declared in the encapsulation header is not a remainder.
#[cfg(feature = "linkable-cdr")]
#[test]
fn plain_codec_links_reject_leftovers_but_accept_declared_cdr_padding() {
    futures::executor::block_on(async {
        use aimdb_data_contracts::link_codecs::Cdr;
        let (db, captured) = db(|r| {
            r.link_from("test://cdr")
                .with_link_codec(Cdr::<32>)
                .finish();
            r.link_from("test://postcard")
                .with_link_codec(Postcard::<32>)
                .finish();
        })
        .await;
        let inbound = InboundDispatch::new(&db, "test", &ExactGrammar).unwrap();
        let exact = Cdr::<32>.encode(&value()).unwrap();
        inbound.dispatch("cdr", &exact);
        // A DDS writer padding to a 4-byte boundary declares it in the options.
        let mut padded = exact.clone();
        padded[3] = 3;
        padded.extend_from_slice(&[0; 3]);
        inbound.dispatch("cdr", &padded);
        assert_eq!(captured.lock().unwrap().len(), 2);
        assert_eq!(inbound.stats(0).unwrap().accepted, 2);

        let mut junk = exact.clone();
        junk.push(0);
        inbound.dispatch("cdr", &junk);
        let mut overclaimed = exact;
        overclaimed[3] = 2;
        overclaimed.push(0);
        inbound.dispatch("cdr", &overclaimed);
        let mut postcard = Postcard::<32>.encode(&value()).unwrap();
        postcard.push(0);
        inbound.dispatch("postcard", &postcard);
        assert_eq!(inbound.stats(0).unwrap().decode_error, 2);
        assert_eq!(inbound.stats(1).unwrap().decode_error, 1);
        assert_eq!(captured.lock().unwrap().len(), 2);
    });
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Large {
    message: String,
}
impl aimdb_data_contracts::SchemaType for Large {
    const NAME: &'static str = "large";
}
impl aimdb_data_contracts::ContractShape for Large {}
aimdb_data_contracts::migration_chain! { type Current = Large; steps {} }

#[test]
fn custom_owned_fallback_has_no_scratch_size_ceiling() {
    futures::executor::block_on(async {
        let key_large_4 = aimdb_data_contracts::Key::<Large>::new("large");
        let mut b = builder();
        b.configure(key_large_4, |r| {
            r.buffer_raw(Box::new(Buffer {
                value: Large {
                    message: "x".repeat(2048),
                },
                captured: Default::default(),
            }));
            r.link_to("test://large")
                .with_versioned_codec(Tagged(42))
                .with_envelope(EnvelopeCarrier::Inline)
                .finish();
        });
        let (db, _) = b.build().await.unwrap();
        let mut routes = OutboundRoutes::new(&db, "test").unwrap();
        let msg = routes.next().await.unwrap();
        let env = Envelope::parse(msg.payload.as_slice()).unwrap();
        let (decoded, used) = Tagged(42).decode_payload::<Large>(env.payload()).unwrap();
        assert_eq!(decoded.message, "x".repeat(2048));
        assert_eq!(used, env.payload().len());
        assert!(matches!(msg.payload, OutboundPayload::Owned(_)));
    });
}

#[derive(Clone)]
struct SingleType;
impl LinkCodec<V2> for SingleType {
    const ENCODE_BUFFER_CAPACITY: Option<usize> = Some(3);
    fn decode(&self, _: &[u8]) -> Result<V2, String> {
        Ok(value())
    }
    fn encode(&self, _: &V2) -> Result<Vec<u8>, SerializeError> {
        Ok(vec![9, 8, 7])
    }
    fn encode_into(&self, _: &V2, out: &mut [u8]) -> Result<usize, SerializeError> {
        out.get_mut(..3)
            .ok_or(SerializeError::BufferTooSmall)?
            .copy_from_slice(&[9, 8, 7]);
        Ok(3)
    }
}
#[test]
fn foreign_single_type_codec_stays_bare_and_cannot_claim_versioned_support() {
    futures::executor::block_on(async {
        let key_reading_5 = aimdb_data_contracts::Key::<V2>::new("reading");
        let mut b = builder().with_connector(Noop("knx"));
        b.configure(key_reading_5, |r| {
            r.buffer_raw(Box::new(Buffer {
                value: value(),
                captured: Default::default(),
            }));
            r.link_to("knx://foreign")
                .with_link_codec(SingleType)
                .finish();
        });
        let (db, _) = b.build().await.unwrap();
        let mut routes = OutboundRoutes::new(&db, "knx").unwrap();
        assert_eq!(routes.next().await.unwrap().payload.as_slice(), [9, 8, 7]);
    });
    assert!(invalid(|r| {
        r.link_to("test://out")
            .with_link_codec(SingleType)
            .with_envelope(EnvelopeCarrier::Inline)
            .finish();
    })
    .contains("select a builtin codec"));
}

#[test]
fn typed_keys_keep_schema_on_downgrade_and_plain_names_stay_schema_free() {
    const CONTRACT: aimdb_data_contracts::Key<V2> =
        aimdb_data_contracts::Key::new("contract").with_link("test://contract");
    const PLAIN: aimdb_core::Name<V2> = aimdb_core::Name::new("plain");
    futures::executor::block_on(async {
        let mut b = builder();
        b.configure(CONTRACT, |r| {
            r.buffer_raw(Box::new(Buffer {
                value: value(),
                captured: Default::default(),
            }));
            r.link_to_key()
                .with_link_codec(Postcard::<32>)
                .sends_as::<1>()
                .finish();
        });
        b.configure(PLAIN, |r| {
            r.buffer_raw(Box::new(Buffer {
                value: value(),
                captured: Default::default(),
            }));
            r.link_to("test://plain")
                .with_link_codec(Postcard::<32>)
                .sends_as::<1>()
                .finish();
        });
        let (db, _) = b.build().await.unwrap();
        let graph = db.inner().dependency_graph();
        assert_eq!(
            graph
                .node("contract")
                .unwrap()
                .schema
                .as_ref()
                .unwrap()
                .version,
            2
        );
        assert!(graph.node("plain").unwrap().schema.is_none());
        let metadata = db.list_records();
        assert_eq!(
            metadata
                .iter()
                .find(|m| m.record_key == "contract")
                .unwrap()
                .schema_version,
            Some(2)
        );
        assert_eq!(
            metadata
                .iter()
                .find(|m| m.record_key == "plain")
                .unwrap()
                .schema_version,
            None
        );
    });
}

#[test]
fn key_link_without_address_fails_at_build() {
    const RECORD: aimdb_data_contracts::Key<V2> = aimdb_data_contracts::Key::new("missing");
    futures::executor::block_on(async {
        let mut b = builder();
        b.configure(RECORD, |r| {
            r.buffer_raw(Box::new(Buffer {
                value: value(),
                captured: Default::default(),
            }));
            r.link_to_key().with_link_codec(Postcard::<32>).finish();
        });
        assert!(b.build().await.is_err());
    });
}
