//! `InboundLinkBuilder`: registration (`finish`) and the ingest path
//! (`InboundDispatch`, stamping, admission counters).

use super::*;

// ====================================================================
// Inbound link registration tests (fused ingest)
// ====================================================================

#[test]
fn inbound_finish_registers_fused_link() {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));

    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();

    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    reg.link_from("mqtt://broker/topic")
        .with_deserializer(|_ctx, bytes: &[u8]| {
            Ok(TestRecord {
                value: bytes.len() as i32,
            })
        })
        .finish();

    assert_eq!(rec.inbound_connectors().len(), 1);
    assert_eq!(rec.inbound_connectors()[0].resolve_topic(), "broker/topic");
    assert!(drain_errors(&mut rec).is_empty());
}

#[test]
fn inbound_finish_without_deserializer_records_error() {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));

    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();

    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    // No deserializer set — error recorded, link not registered
    reg.link_from("mqtt://broker/topic").finish();

    assert!(rec.inbound_connectors().is_empty());
    let errors = drain_errors(&mut rec);
    assert_eq!(errors.len(), 1);
    assert!(errors[0]
        .message
        .contains("Inbound connector requires a deserializer"));
    assert_eq!(errors[0].record_key, "test::Record");
    assert_eq!(errors[0].url.as_deref(), Some("mqtt://broker/topic"));
}

// ====================================================================
// Topic patterns and keys on inbound links
// ====================================================================

/// Runs `configure` against a fresh registrar with an `mqtt` connector
/// and returns the record's links and recorded errors.
fn register(
    configure: impl FnOnce(&mut RecordRegistrar<'_, TestRecord>),
) -> (
    Vec<crate::connector::InboundConnectorLink>,
    Vec<crate::error::ConfigError>,
) {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));
    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();
    configure(&mut make_registrar(&mut rec, &builders, &extensions));
    (rec.inbound_connectors().to_vec(), drain_errors(&mut rec))
}

fn match_deser(
    _ctx: &crate::RuntimeContext,
    m: &crate::TopicMatch<'_>,
    _bytes: &[u8],
) -> Result<TestRecord, crate::IngestError> {
    Ok(TestRecord {
        value: m.topic().len() as i32,
    })
}

#[test]
fn inbound_finish_registers_match_link_with_key() {
    let (links, errors) = register(|reg| {
        reg.link_from("mqtt://s/{device}/t")
            .key("device", 16)
            .with_match_deserializer(match_deser)
            .finish();
    });
    assert!(errors.is_empty(), "{errors:?}");
    let (capture, capacity) = links[0].key.clone().unwrap();
    assert_eq!((capture.as_str(), capacity.get()), ("device", 16));
}

#[test]
fn inbound_finish_rejects_both_deserializers() {
    let (links, errors) = register(|reg| {
        reg.link_from("mqtt://s/{device}/t")
            .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
            .with_match_deserializer(match_deser)
            .finish();
    });
    assert!(links.is_empty());
    assert!(errors[0].message.contains("not both"), "{:?}", errors[0]);
}

#[test]
fn inbound_finish_rejects_invalid_pattern_syntax() {
    for topic in [
        "s/{d",
        "s/{}",
        "s/{d-1}",
        "{d}/{d}",
        "{a}/{b}/{c}/{d}/{e}/{f}/{g}/{h}/{i}",
    ] {
        let url = alloc::format!("mqtt://{topic}");
        let (links, errors) = register(|reg| {
            reg.link_from(&url)
                .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
                .finish();
        });
        assert!(links.is_empty(), "{topic}");
        assert_eq!(errors.len(), 1, "{topic}");
        assert_eq!(errors[0].record_key, "test::Record");
        assert_eq!(errors[0].url.as_deref(), Some(url.as_str()));
    }
}

#[test]
fn inbound_finish_rejects_bad_keys() {
    for (topic, key, capacity, needle) in [
        ("s/{d}/t", "device", 4, "not a capture"),
        ("s/{d}/t", "d", 0, "at least 1"),
    ] {
        let (links, errors) = register(|reg| {
            reg.link_from(&alloc::format!("mqtt://{topic}"))
                .key(key, capacity)
                .with_match_deserializer(match_deser)
                .finish();
        });
        assert!(links.is_empty());
        assert!(errors[0].message.contains(needle), "{:?}", errors[0]);
    }
}

#[test]
fn keyed_links_of_one_record_share_a_capacity() {
    let (links, errors) = register(|reg| {
        reg.link_from("mqtt://temp/{d}")
            .key("d", 8)
            .with_match_deserializer(match_deser)
            .finish();
        reg.link_from("mqtt://hum/{id}")
            .key("id", 8)
            .with_match_deserializer(match_deser)
            .finish();
        reg.link_from("mqtt://co2/{id}")
            .key("id", 16)
            .with_match_deserializer(match_deser)
            .finish();
    });
    assert_eq!(links.len(), 2);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("has 8"), "{:?}", errors[0]);
}

/// Dispatches `payload` on `topic` through the `mqtt` inbound routes.
fn route(db: &crate::AimDb, topic: &str, payload: &[u8]) {
    crate::InboundDispatch::new(db, "mqtt", &Plus)
        .expect("routes compile")
        .dispatch(topic, payload);
}

/// End-to-end inbound path: bytes → fused ingest → typed buffer push,
/// with no `Box<dyn Any>` in between.
#[tokio::test]
async fn ingest_roundtrip_produces_value() {
    let (db, last, count) = inbound_db(|reg| {
        reg.link_from("mqtt://cmd/in")
            .with_deserializer(|_ctx, bytes: &[u8]| {
                if bytes.is_empty() {
                    return Err("empty payload".into());
                }
                Ok(TestRecord {
                    value: bytes.len() as i32,
                })
            })
            .finish();
    })
    .await;

    route(&db, "cmd/in", &[1, 2, 3]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(last.load(Ordering::SeqCst), 3);

    // Bad bytes: the deserializer fails, nothing is produced.
    route(&db, "cmd/in", &[]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

/// The deserializer set last wins, whichever way its context is typed.
#[tokio::test]
async fn deserializer_set_last_wins() {
    let (db, last, _) = inbound_db(|reg| {
        reg.link_from("mqtt://cmd/in")
            .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
            .with_deserializer(|_ctx: crate::RuntimeContext, _bytes: &[u8]| {
                Ok(TestRecord { value: 99 })
            })
            .finish();
    })
    .await;
    route(&db, "cmd/in", b"x");
    assert_eq!(last.load(Ordering::SeqCst), 99);
}

/// Buffer that records the stamp of the last pushed value.
struct StampRecordingBuffer(Arc<core::sync::atomic::AtomicU64>);

impl crate::buffer::DynBuffer<crate::Stamped<TestRecord>> for StampRecordingBuffer {
    fn push(&self, sample: crate::Stamped<TestRecord>) {
        self.0.store(sample.stamp().as_nanos(), Ordering::SeqCst);
    }
    fn subscribe_boxed(
        &self,
    ) -> Box<dyn crate::buffer::BufferReader<crate::Stamped<TestRecord>> + Send> {
        unimplemented!("not needed for ingest tests")
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// Design 058 §1: an inbound value is stamped when it is written locally.
#[tokio::test]
async fn an_inbound_value_is_stamped_when_written() {
    let key_rec_in_12 = crate::Name::<TestRecord>::new("rec.in");
    let ops = crate::executor::test_support::ManualClockRuntimeOps::default();
    let stamp = Arc::new(core::sync::atomic::AtomicU64::new(u64::MAX));
    let buf_stamp = stamp.clone();
    let mut builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(ops.clone()))
        .with_connector(NoopConnectorBuilder);
    builder.configure(key_rec_in_12, move |reg| {
        reg.buffer_raw(Box::new(StampRecordingBuffer(buf_stamp)));
        reg.link_from("mqtt://cmd/in")
            .with_deserializer(|_ctx, bytes: &[u8]| {
                Ok::<_, crate::IngestError>(TestRecord {
                    value: bytes.len() as i32,
                })
            })
            .finish();
    });
    let (db, _runner) = builder.build().await.expect("build must succeed");

    ops.clock.advance(core::time::Duration::from_nanos(4242));
    route(&db, "cmd/in", &[1]);
    assert_eq!(stamp.load(Ordering::SeqCst), 4242);
}

#[tokio::test]
async fn inbound_dispatch_routes_patterns_with_shared_keys() {
    let keys: Arc<spin::Mutex<Vec<crate::KeyId>>> = Default::default();
    let seen = keys.clone();
    let (db, last, count) = inbound_db(move |reg| {
        reg.link_from("mqtt://temp/{d}")
            .key("d", 2)
            .with_match_deserializer(move |ctx, m, bytes| {
                seen.lock().extend(m.key());
                key_deser(ctx, m, bytes)
            })
            .finish();
        reg.link_from("mqtt://hum/{id}")
            .key("id", 2)
            .with_match_deserializer(key_deser)
            .finish();
        reg.link_from("mqtt://cmd/in")
            .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 100 }))
            .finish();
    })
    .await;

    let inbound = crate::InboundDispatch::new(&db, "mqtt", &Plus).expect("routes compile");
    let subscriptions: Vec<String> = inbound
        .subscriptions()
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(subscriptions, ["cmd/in", "hum/+", "temp/+"]);
    assert_eq!(inbound.route_count(), 3);

    let dispatch = |topic: &str| {
        inbound.dispatch(topic, b"");
        last.load(Ordering::SeqCst)
    };
    assert_eq!(dispatch("temp/a"), 0);
    assert_eq!(dispatch("hum/b"), 1);
    assert_eq!(dispatch("hum/a"), 0, "one key table per record");
    assert_eq!(dispatch("cmd/in"), 100);
    let produced = count.load(Ordering::SeqCst);
    dispatch("temp/c");
    assert_eq!(count.load(Ordering::SeqCst), produced, "full table drops");

    let a = keys.lock()[0];
    assert_eq!(db.inbound_key_name("rec.in", a).as_deref(), Some("a"));
    assert_eq!(db.inbound_key_name("other", a), None);

    #[cfg(feature = "remote")]
    {
        let records = db.list_records();
        let info = records[0].inbound_keys.as_ref().expect("keyed record");
        assert_eq!(info.captures, ["d", "id"]);
        assert_eq!((info.capacity, info.assigned, info.dropped), (2, 2, 1));
    }
}

#[tokio::test]
async fn inbound_dispatch_rejects_links_it_cannot_compile() {
    let (db, _, _) = inbound_db(|reg| {
        reg.link_from("mqtt://s/{d}/t")
            .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();
        reg.link_from("mqtt://r/one")
            .with_topic_resolver(|| Some("r/{d".into()))
            .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();
        reg.link_from("mqtt://k/{d}")
            .key("d", 4)
            .with_topic_resolver(|| Some("k/{x}".into()))
            .with_match_deserializer(key_deser)
            .finish();
    })
    .await;

    let errors = config_errors(crate::InboundDispatch::new(&db, "mqtt", &Plus));
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().all(|e| e.record_key == "rec.in"));
    assert!(errors[0].message.contains("unbalanced '{' in 'r/{d'"));
    assert!(errors[1]
        .message
        .contains("key 'd' is not a capture of 'k/{x}'"));

    let errors = config_errors(crate::InboundDispatch::new(
        &db,
        "mqtt",
        &crate::ExactGrammar,
    ));
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(errors[0]
        .message
        .contains("does not support topic patterns"));
}

#[tokio::test]
async fn inbound_dispatch_clones_share_records() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<crate::InboundDispatch>();

    let (db, last, count) = inbound_db(|reg| {
        reg.link_from("mqtt://cmd/in")
            .with_deserializer(|_ctx, bytes: &[u8]| {
                Ok(TestRecord {
                    value: bytes.len() as i32,
                })
            })
            .finish();
    })
    .await;

    let inbound = crate::InboundDispatch::new(&db, "mqtt", &Plus).unwrap();
    let clone = inbound.clone();
    drop(inbound);
    clone.dispatch("cmd/in", b"abcd");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(last.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn admission_outcomes_are_counted_after_production_and_survive_reconnect() {
    let (db, last, count) = inbound_db(|reg| {
        reg.link_from("mqtt://cmd/in")
            .with_admitting_deserializer(&[1], |_ctx, _m, bytes| {
                if bytes.is_empty() {
                    return Err(crate::IngestError::new(
                        crate::Rejection::TooOld,
                        "received v0, accepts v1..=2",
                    ));
                }
                Ok(crate::IngestValue::upgraded(TestRecord { value: 42 }, 1))
            })
            .finish();
    })
    .await;
    let inbound = crate::InboundDispatch::new(&db, "mqtt", &Plus).unwrap();
    let clone = inbound.clone();
    assert_eq!(inbound.routes()[0].id, 0);
    clone.dispatch("cmd/in", b"v1");
    assert_eq!(last.load(Ordering::SeqCst), 42);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    inbound.dispatch("cmd/in", b"");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let snapshot = clone.stats(0).unwrap();
    assert_eq!(snapshot.accepted, 1);
    assert_eq!(snapshot.too_old, 1);
    assert_eq!(
        snapshot.upgraded[0],
        crate::UpgradeCount { from: 1, count: 1 }
    );
    assert_eq!(inbound.stats(0).unwrap(), snapshot);
    drop(inbound);
    drop(clone);
    let reconnected = crate::InboundDispatch::new(&db, "mqtt", &Plus).unwrap();
    assert_eq!(reconnected.stats(0).unwrap(), snapshot);
    reconnected.dispatch("cmd/in", b"v1");
    assert_eq!(reconnected.stats(0).unwrap().upgraded[0].count, 2);
    #[cfg(all(feature = "remote", feature = "observability"))]
    {
        let records = db.list_records();
        let metrics = &records
            .iter()
            .find(|r| r.record_key == "rec.in")
            .unwrap()
            .inbound_routes;
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].link, "mqtt://cmd/in");
        assert_eq!(metrics[0].stats, reconnected.stats(0).unwrap());
        let json = serde_json::to_value(&records).unwrap();
        assert!(json.to_string().contains("too_old"));
    }
}
