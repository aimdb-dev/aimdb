//! `OutboundLinkBuilder`: registration (`finish`, serializers, wire format)
//! and the pull path (`OutboundRoutes`, scratch, topic writers).

use super::*;

#[test]
fn outbound_finish_rejects_patterns() {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));
    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();
    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    reg.link_to("mqtt://out/{device}")
        .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec()))
        .finish();

    assert!(rec.outbound_connectors().is_empty());
    let errors = drain_errors(&mut rec);
    assert!(errors[0].message.contains("cannot contain '{' or '}'"));
}

// ====================================================================
// Outbound link registration tests (fused source)
// ====================================================================

#[test]
fn outbound_finish_registers_fused_link() {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));

    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();

    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    reg.link_to("mqtt://broker/topic")
        .with_serializer(|_ctx, record: &TestRecord| Ok(record.value.to_le_bytes().to_vec()))
        .finish();

    assert_eq!(rec.outbound_connectors().len(), 1);
    assert_eq!(
        rec.outbound_connectors()[0].url.resource_id(),
        "broker/topic"
    );
    assert!(drain_errors(&mut rec).is_empty());
}

#[test]
fn outbound_finish_without_serializer_records_error() {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));

    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();

    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    // No serializer set — error recorded, link not registered
    reg.link_to("mqtt://broker/topic").finish();

    assert!(rec.outbound_connectors().is_empty());
    let errors = drain_errors(&mut rec);
    assert_eq!(errors.len(), 1);
    assert!(errors[0]
        .message
        .contains("Outbound connector requires a serializer"));
    assert_eq!(errors[0].record_key, "test::Record");
    assert_eq!(errors[0].url.as_deref(), Some("mqtt://broker/topic"));
}

// ====================================================================
// OutboundRoutes over typed links
// ====================================================================

/// Buffer reader that replays a fixed script, then reports the buffer
/// closed.
struct ScriptedReader {
    script: Vec<Result<TestRecord, crate::DbError>>,
}

impl crate::buffer::BufferReader<crate::Stamped<TestRecord>> for ScriptedReader {
    fn poll_recv(
        &mut self,
        _cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Result<crate::Stamped<TestRecord>, crate::DbError>> {
        let next = if self.script.is_empty() {
            Err(crate::DbError::BufferClosed {
                buffer_name: "scripted".to_string(),
            })
        } else {
            self.script.remove(0)
        };
        core::task::Poll::Ready(next.map(|value| {
            crate::Stamped::new(
                value,
                crate::WriteMetadata::new(crate::Stamp::from_nanos(0)),
            )
        }))
    }
    fn try_recv(&mut self) -> Result<crate::Stamped<TestRecord>, crate::DbError> {
        unimplemented!("not needed for outbound route tests")
    }
}

type Script = fn() -> Vec<Result<TestRecord, crate::DbError>>;

/// Buffer whose readers replay `script`.
struct ScriptedBuffer(Script);

impl crate::buffer::DynBuffer<crate::Stamped<TestRecord>> for ScriptedBuffer {
    fn push(&self, _value: crate::Stamped<TestRecord>) {}
    fn subscribe_boxed(
        &self,
    ) -> Box<dyn crate::buffer::BufferReader<crate::Stamped<TestRecord>> + Send> {
        Box::new(ScriptedReader { script: (self.0)() })
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

fn values(values: &[i32]) -> Vec<Result<TestRecord, crate::DbError>> {
    values
        .iter()
        .map(|&value| Ok(TestRecord { value }))
        .collect()
}

/// Writes `r` into `out`.
fn le_into(
    _ctx: crate::RuntimeContext,
    r: &TestRecord,
    out: &mut [u8],
) -> Result<usize, SerializeError> {
    let bytes = r.value.to_le_bytes();
    out.get_mut(..bytes.len())
        .ok_or(SerializeError::BufferTooSmall)?
        .copy_from_slice(&bytes);
    Ok(bytes.len())
}

/// `OutboundRoutes` for the `mqtt` links `links` registers on a record
/// whose buffer replays `script`.
async fn outbound(
    script: Script,
    links: impl FnOnce(&mut RecordRegistrar<'_, TestRecord>) + Send + 'static,
) -> (crate::AimDb, crate::OutboundRoutes) {
    let key_rec_out_13 = crate::Name::<TestRecord>::new("rec.out");
    let mut builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(NoopConnectorBuilder);
    builder.configure(key_rec_out_13, move |reg| {
        reg.buffer_raw(Box::new(ScriptedBuffer(script)));
        links(reg);
    });
    let (db, _runner) = builder.build().await.expect("build must succeed");
    let outbound = crate::OutboundRoutes::new(&db, "mqtt").expect("outbound routes");
    (db, outbound)
}

/// The next message as `(topic, value, owned)`.
async fn pull(o: &mut crate::OutboundRoutes) -> Option<(String, i32, bool)> {
    let msg = o.next().await?;
    let owned = matches!(msg.payload, crate::OutboundPayload::Owned(_));
    let bytes: [u8; 4] = msg.payload.as_slice().try_into().expect("4 bytes");
    Some((msg.topic.to_string(), i32::from_le_bytes(bytes), owned))
}

/// A lag is counted and skipped; a closed buffer ends the route.
#[tokio::test]
async fn outbound_routes_count_lag_and_end_on_close() {
    let (_db, mut o) = outbound(
        || {
            vec![
                Ok(TestRecord { value: 1 }),
                Err(lagged()),
                Ok(TestRecord { value: 2 }),
            ]
        },
        |reg| {
            reg.link_to("mqtt://tele/out").with_serializer(le).finish();
        },
    )
    .await;

    use crate::outbound::events::{capture, OutboundEvent};
    let seen = Arc::new(spin::Mutex::new(Vec::new()));
    let observer = {
        let seen = seen.clone();
        Arc::new(move |event: &OutboundEvent<'_>| {
            seen.lock().push(match event {
                OutboundEvent::Lagged { topic, skipped } => {
                    assert_eq!((*topic, *skipped), ("tele/out", 1));
                    "lag"
                }
                OutboundEvent::Closed { topic, error } => {
                    assert_eq!(*topic, "tele/out");
                    assert!(matches!(error, crate::DbError::BufferClosed { .. }));
                    "closed"
                }
                other => panic!("unexpected {other:?}"),
            });
        })
    };
    capture::future(observer, async {
        assert_eq!(pull(&mut o).await, Some(("tele/out".into(), 1, true)));
        assert_eq!(pull(&mut o).await, Some(("tele/out".into(), 2, true)));
        assert_eq!(o.stats(0).unwrap().lagged, 1);
        assert_eq!(pull(&mut o).await, None);
    })
    .await;
    assert_eq!(*seen.lock(), ["lag", "closed"]);
}

fn lagged() -> crate::DbError {
    crate::DbError::BufferLagged {
        lag_count: 1,
        buffer_name: "scripted".to_string(),
    }
}

/// A value whose serializer fails is counted and skipped.
#[tokio::test]
async fn outbound_routes_skip_serialize_failures() {
    let (_db, mut o) = outbound(
        || values(&[13, 42]),
        |reg| {
            reg.link_to("mqtt://tele/out")
                .with_serializer(|ctx, r: &TestRecord| {
                    if r.value == 13 {
                        return Err(SerializeError::InvalidData);
                    }
                    le(ctx, r)
                })
                .finish();
        },
    )
    .await;

    assert_eq!(pull(&mut o).await.map(|m| m.1), Some(42));
    assert_eq!(o.stats(0).unwrap().serialize_failed, 1);
}

#[tokio::test]
async fn outbound_routes_use_scratch_without_owned_fallback() {
    let owned_calls = Arc::new(AtomicUsize::new(0));
    let into_calls = Arc::new(AtomicUsize::new(0));
    let (owned_counter, into_counter) = (owned_calls.clone(), into_calls.clone());
    let (_db, mut o) = outbound(
        || values(&[7]),
        move |reg| {
            reg.link_to("mqtt://tele/out")
                .with_serializer(move |ctx, r: &TestRecord| {
                    owned_counter.fetch_add(1, Ordering::SeqCst);
                    le(ctx, r)
                })
                .with_serializer_into(8, move |ctx, r: &TestRecord, out| {
                    into_counter.fetch_add(1, Ordering::SeqCst);
                    le_into(ctx, r, out)
                })
                .finish();
        },
    )
    .await;

    assert_eq!(pull(&mut o).await, Some(("tele/out".into(), 7, false)));
    assert_eq!(into_calls.load(Ordering::SeqCst), 1);
    assert_eq!(owned_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn outbound_routes_fall_back_once_when_scratch_is_small() {
    let owned_calls = Arc::new(AtomicUsize::new(0));
    let owned_counter = owned_calls.clone();
    let (_db, mut o) = outbound(
        || values(&[9]),
        move |reg| {
            reg.link_to("mqtt://tele/out")
                .with_serializer(move |ctx, r: &TestRecord| {
                    owned_counter.fetch_add(1, Ordering::SeqCst);
                    le(ctx, r)
                })
                .with_serializer_into(2, |_ctx, _r: &TestRecord, _out| {
                    Err(SerializeError::BufferTooSmall)
                })
                .finish();
        },
    )
    .await;

    assert_eq!(pull(&mut o).await, Some(("tele/out".into(), 9, true)));
    assert_eq!(owned_calls.load(Ordering::SeqCst), 1);
}

/// An into-slice serializer that reports more bytes than the scratch
/// holds, or fails with anything but `BufferTooSmall`, skips the value.
#[tokio::test]
async fn outbound_routes_skip_invalid_lengths_and_data() {
    let (_db, mut o) = outbound(
        || values(&[1, 2, 3]),
        |reg| {
            reg.link_to("mqtt://tele/out")
                .with_serializer(le)
                .with_serializer_into(8, |ctx, r: &TestRecord, out| match r.value {
                    1 => Ok(out.len() + 1),
                    2 => Err(SerializeError::InvalidData),
                    _ => le_into(ctx, r, out),
                })
                .finish();
        },
    )
    .await;

    assert_eq!(pull(&mut o).await, Some(("tele/out".into(), 3, false)));
    assert_eq!(o.stats(0).unwrap().serialize_failed, 2);
}

/// `with_topic_fn` infers an unannotated closure's argument types;
/// `Ok(false)` publishes to the link's default topic.
#[tokio::test]
async fn with_topic_fn_writes_the_topic_or_the_default() {
    use core::fmt::Write as _;
    let (_db, mut o) = outbound(
        || values(&[5, 0]),
        |reg| {
            reg.link_to("mqtt://tele/out")
                .with_topic_fn(8, |v, out| {
                    if v.value <= 0 {
                        return Ok(false);
                    }
                    write!(out, "dyn/{}", v.value)?;
                    Ok(true)
                })
                .with_serializer(le)
                .finish();
        },
    )
    .await;

    assert_eq!(o.routes()[0].topic_capacity, 8);
    assert_eq!(pull(&mut o).await.map(|m| m.0), Some("dyn/5".into()));
    assert_eq!(pull(&mut o).await.map(|m| m.0), Some("tele/out".into()));
}

/// A topic that overflows skips its value, even when the writer ignores
/// the error and returns `Ok(true)`.
#[tokio::test]
async fn overflowing_topics_are_skipped() {
    use core::fmt::Write as _;
    let (_db, mut o) = outbound(
        || values(&[123_456, 1_234_567, 7]),
        |reg| {
            reg.link_to("mqtt://tele/out")
                .with_topic_fn(6, |v, out| {
                    if v.value == 1_234_567 {
                        let _ = write!(out, "t/{}", v.value);
                        return Ok(true);
                    }
                    write!(out, "t/{}", v.value)?;
                    Ok(true)
                })
                .with_serializer(le)
                .finish();
        },
    )
    .await;

    // "t/123456" (8 bytes) and "t/1234567" (9 bytes) do not fit in 6.
    assert_eq!(pull(&mut o).await, Some(("t/7".into(), 7, true)));
    assert_eq!(o.stats(0).unwrap().topic_overflow, 2);
}

/// Registrar → build → `OutboundRoutes`, pinning the factory wiring: the
/// serializer set last wins, whichever way its context is typed.
#[tokio::test]
async fn outbound_roundtrip_yields_serialized_values() {
    let (_db, mut o) = outbound(
        || values(&[5]),
        |reg| {
            reg.link_to("mqtt://tele/out?qos=1")
                .with_serializer(|_ctx, _r: &TestRecord| Ok(vec![0]))
                .with_serializer(|_ctx: crate::RuntimeContext, r: &TestRecord| {
                    Ok(r.value.to_le_bytes().to_vec())
                })
                .with_serializer_into(4, |ctx, r: &TestRecord, out| le_into(ctx, r, out))
                .finish();
        },
    )
    .await;

    let route = &o.routes()[0];
    assert_eq!(&*route.default_topic, "tele/out");
    assert_eq!(route.payload_capacity, 4);
    assert_eq!(route.record_index, 0);
    assert_eq!(pull(&mut o).await, Some(("tele/out".into(), 5, false)));
    assert_eq!(pull(&mut o).await, None);
}

#[test]
fn serializer_setters_clear_recorded_wire_format() {
    use crate::connector::WIRE_FORMAT_KEY;

    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));
    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();
    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    reg.link_to("mqtt://a")
        .with_config(WIRE_FORMAT_KEY, "cdr")
        .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec()))
        .finish();
    reg.link_to("mqtt://b")
        .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec()))
        .with_config(WIRE_FORMAT_KEY, "cdr")
        .with_serializer_into(4, |_ctx, _r: &TestRecord, _out| Ok(0))
        .finish();
    reg.link_from("mqtt://c")
        .with_config(WIRE_FORMAT_KEY, "cdr")
        .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
        .finish();
    reg.link_from("mqtt://e")
        .with_config(WIRE_FORMAT_KEY, "cdr")
        .with_match_deserializer(|_ctx, _m, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
        .finish();
    reg.link_to("mqtt://d")
        .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec()))
        .with_config(WIRE_FORMAT_KEY, "cdr")
        .finish();

    let has_format = |config: &[(String, String)]| config.iter().any(|(k, _)| k == WIRE_FORMAT_KEY);
    let outbound = rec.outbound_connectors();
    assert!(!has_format(&outbound[0].config));
    assert!(!has_format(&outbound[1].config));
    assert!(has_format(&outbound[2].config), "recorded after the setter");
    let inbound = rec.inbound_connectors();
    assert!(!has_format(&inbound[0].config));
    assert!(!has_format(&inbound[1].config));
}

/// Connectors read each route's record type and options from its info;
/// a topic writer shows as a non-zero `topic_capacity`.
#[tokio::test]
async fn route_infos_carry_type_config_and_resolved_topic() {
    use core::any::TypeId;

    let (_db, outbound) = outbound(
        || values(&[]),
        |reg| {
            reg.link_to("mqtt://tele/fixed")
                .with_serializer(le)
                .with_config("qos", "1")
                .finish();
            reg.link_to("mqtt://tele/written")
                .with_topic_fn(16, |_v, _out| Ok(false))
                .with_serializer(le)
                .finish();
        },
    )
    .await;
    let routes = outbound.routes();
    assert_eq!(routes.len(), 2);
    assert!(routes
        .iter()
        .all(|r| r.type_id == TypeId::of::<TestRecord>()));
    assert_eq!(
        routes[0].config.protocol_options,
        vec![("qos".to_string(), "1".to_string())]
    );
    assert_eq!(routes[0].topic_capacity, 0);
    assert_eq!(routes[1].topic_capacity, 16);

    let resolved = Arc::new(AtomicUsize::new(0));
    let calls = resolved.clone();
    let (db, _last, _count) = inbound_db(move |reg| {
        reg.link_from("mqtt://cmd/in")
            .with_topic_resolver(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Some("cmd/resolved".to_string())
            })
            .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 }))
            .with_config("qos", "2")
            .finish();
        reg.link_from("mqtt://dev/{id}")
            .with_match_deserializer(key_deser)
            .finish();
    })
    .await;
    let inbound = crate::InboundDispatch::new(&db, "mqtt", &Plus).expect("routes compile");
    assert_eq!(resolved.load(Ordering::SeqCst), 1, "resolved once");

    let infos = inbound.routes();
    assert_eq!(infos.len(), inbound.route_count());
    let topics: Vec<&str> = infos.iter().map(|i| &*i.topic).collect();
    assert_eq!(topics, ["cmd/resolved", "dev/{id}"]);
    assert!(infos
        .iter()
        .all(|i| i.type_id == TypeId::of::<TestRecord>()));
    assert!(infos.iter().all(|i| i.record_index == 0));
    assert_eq!(
        infos[0].config.protocol_options,
        vec![("qos".to_string(), "2".to_string())]
    );
}
