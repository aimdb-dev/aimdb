//! `RecordRegistrar`: link registration and validation shared by both
//! directions (connector handles, detached links, envelopes, stage rules).

use super::*;

/// A detached link keeps its options when registered: `push_outbound` /
/// `push_inbound` must copy the config from the user's link, not from
/// the link they build.
#[test]
fn detached_links_keep_their_config() {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));
    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();
    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    reg.push_outbound(
        OutboundLink::new("mqtt://out")
            .with_config("qos", "1")
            .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec())),
    );
    reg.push_inbound(
        InboundLink::new("mqtt://in")
            .with_config("qos", "2")
            .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 })),
    );

    let qos = |config: &[(String, String)]| {
        config
            .iter()
            .find(|(k, _)| k == "qos")
            .map(|(_, v)| v.clone())
    };
    assert_eq!(
        qos(&rec.outbound_connectors()[0].config).as_deref(),
        Some("1")
    );
    assert_eq!(
        qos(&rec.inbound_connectors()[0].config).as_deref(),
        Some("2")
    );
    assert!(drain_errors(&mut rec).is_empty());
}

#[test]
fn envelope_config_is_consumed_by_core_and_never_reaches_connectors() {
    use crate::link_wire::ENVELOPE_KEY;
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));
    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();
    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    reg.push_outbound(
        OutboundLink::new("mqtt://out")
            .with_config(ENVELOPE_KEY, "inline")
            .with_config("qos", "1")
            .with_config(ENVELOPE_KEY, "bound")
            .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec())),
    );
    reg.push_inbound(
        InboundLink::new("mqtt://in")
            .with_config(ENVELOPE_KEY, "bound")
            .with_config("qos", "2")
            .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 })),
    );
    // Still read first: the last entry wins, and inline needs a finalizer.
    reg.push_inbound(
        InboundLink::new("mqtt://inline")
            .with_config(ENVELOPE_KEY, "inline")
            .with_deserializer(|_ctx, _bytes: &[u8]| Ok(TestRecord { value: 0 })),
    );

    let expected = [("qos".to_string(), "1".to_string())];
    assert_eq!(rec.outbound_connectors()[0].config, expected);
    let expected = [("qos".to_string(), "2".to_string())];
    assert_eq!(rec.inbound_connectors()[0].config, expected);
    assert_eq!(rec.inbound_connectors().len(), 1);
    let errors = drain_errors(&mut rec);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("inline envelopes require"));
}

// ====================================================================
// Detached links via .input() / .output()
// ====================================================================

/// Runs `wire` on a fresh buffered `TestRecord` with an `mqtt` connector
/// registered, and returns the record.
fn registered(
    wire: impl FnOnce(&mut RecordRegistrar<'_, TestRecord>),
) -> crate::typed_record::TypedRecord<TestRecord> {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));
    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();
    wire(&mut make_registrar(&mut rec, &builders, &extensions));
    rec
}

fn match_de(
    _ctx: &crate::RuntimeContext,
    _m: &crate::TopicMatch<'_>,
    _bytes: &[u8],
) -> Result<TestRecord, crate::IngestError> {
    Ok(TestRecord { value: 0 })
}

/// The same options, applied to an attached or a detached builder.
fn outbound_options<R>(
    b: OutboundLinkBuilder<TestRecord, R>,
) -> OutboundLinkBuilder<TestRecord, R> {
    b.with_config("qos", "1")
        .with_serializer(ser)
        .with_config(crate::connector::WIRE_FORMAT_KEY, "le")
        .with_timeout_ms(50)
}

fn inbound_options<R>(b: InboundLinkBuilder<TestRecord, R>) -> InboundLinkBuilder<TestRecord, R> {
    b.with_config("qos", "1")
        .with_match_deserializer(match_de)
        .with_config(crate::connector::WIRE_FORMAT_KEY, "le")
        .key("d", 4)
        .with_topic_resolver(|| None)
}

#[test]
fn detached_inbound_link_registers_like_link_from() {
    let mut attached = registered(|reg| {
        inbound_options(reg.link_from("mqtt://temp/{d}")).finish();
    });
    let mut detached = registered(|reg| {
        reg.input(inbound_options(InboundLink::new("mqtt://temp/{d}")));
    });

    let (a, d) = (attached.inbound_connectors(), detached.inbound_connectors());
    assert_eq!((a.len(), d.len()), (1, 1));
    assert_eq!(a[0].url.to_string(), d[0].url.to_string());
    assert_eq!(a[0].config, d[0].config);
    assert!(d[0].config.iter().any(|(k, v)| k == "qos" && v == "1"));
    assert_eq!(a[0].key, d[0].key);
    assert!(d[0].key.is_some());
    assert!(a[0].topic_resolver.is_some() && d[0].topic_resolver.is_some());
    assert!(drain_errors(&mut attached).is_empty());
    assert!(drain_errors(&mut detached).is_empty());
}

#[test]
fn detached_outbound_links_register_like_link_to() {
    let mut attached = registered(|reg| {
        outbound_options(reg.link_to("mqtt://out")).finish();
        reg.with_name("out");
    });
    let mut detached = registered(|reg| {
        reg.output(outbound_options(OutboundLink::new("mqtt://out")))
            .with_name("out");
    });

    let (a, d) = (
        attached.outbound_connectors(),
        detached.outbound_connectors(),
    );
    assert_eq!((a.len(), d.len()), (1, 1));
    assert_eq!(a[0].url.to_string(), d[0].url.to_string());
    assert_eq!(a[0].config, d[0].config);
    assert!(d[0]
        .config
        .iter()
        .any(|(k, v)| k == "timeout_ms" && v == "50"));

    #[cfg(feature = "observability")]
    for rec in [&attached, &detached] {
        let links = rec.profiling().links();
        assert_eq!(links.len(), 1, "one profiling stage per link");
        assert_eq!(links[0].name.as_deref(), Some("out"));
    }
    assert!(drain_errors(&mut attached).is_empty());
    assert!(drain_errors(&mut detached).is_empty());
}

#[test]
fn detached_links_record_the_same_errors_as_attached_ones() {
    /// Links registered and errors recorded (url, message).
    type Outcome = (usize, Vec<(Option<String>, String)>);
    fn outcome(mut rec: crate::typed_record::TypedRecord<TestRecord>) -> Outcome {
        let links = rec.outbound_connectors().len() + rec.inbound_connectors().len();
        let errors = drain_errors(&mut rec)
            .into_iter()
            .map(|e| (e.url, e.message))
            .collect();
        (links, errors)
    }

    // Each case runs once through `link_to`/`link_from` + `finish()`
    // and once through `.output()`/`.input()`; `$b` is the builder.
    macro_rules! same_errors {
        (out $url:expr, |$b:ident| $opts:expr, $msg:expr) => {{
            let attached = outcome(registered(|reg| {
                let $b = reg.link_to($url);
                $opts.finish();
            }));
            let detached = outcome(registered(|reg| {
                let $b = OutboundLink::new($url);
                reg.output($opts);
            }));
            assert_eq!(attached, detached, "outbound {}", $url);
            assert_eq!(detached.0, 0, "nothing registered for {}", $url);
            assert_eq!(detached.1.len(), 1, "one error for {}", $url);
            assert!(detached.1[0].1.contains($msg), "{:?}", detached.1);
        }};
        (in $url:expr, |$b:ident| $opts:expr, $msg:expr) => {{
            let attached = outcome(registered(|reg| {
                let $b = reg.link_from($url);
                $opts.finish();
            }));
            let detached = outcome(registered(|reg| {
                let $b = InboundLink::new($url);
                reg.input($opts);
            }));
            assert_eq!(attached, detached, "inbound {}", $url);
            assert_eq!(detached.0, 0, "nothing registered for {}", $url);
            assert_eq!(detached.1.len(), 1, "one error for {}", $url);
            assert!(detached.1[0].1.contains($msg), "{:?}", detached.1);
        }};
    }

    same_errors!(out "no-scheme", |b| b.with_serializer(ser), "Invalid connector URL");
    same_errors!(out "mqtt://a/{x}", |b| b.with_serializer(ser), "cannot contain");
    same_errors!(out "mqtt://a", |b| b, "requires a serializer");
    same_errors!(out "kafka://a", |b| b.with_serializer(ser), "No connector registered");

    same_errors!(in "no-scheme", |b| b.with_deserializer(de), "Invalid connector URL");
    same_errors!(in "mqtt://a", |b| b, "requires a deserializer");
    same_errors!(
        in "mqtt://a",
        |b| b.with_deserializer(de).with_match_deserializer(match_de),
        "not both"
    );
    same_errors!(in "kafka://a", |b| b.with_deserializer(de), "No connector registered");
    same_errors!(
        in "mqtt://t/{d}",
        |b| b.with_deserializer(de).key("x", 2),
        "is not a capture"
    );
    same_errors!(
        in "mqtt://t/{d}",
        |b| b.with_deserializer(de).key("d", 0),
        "at least 1"
    );
}

#[test]
fn custom_input_wires_a_source() {
    let mut rec = registered(|reg| {
        reg.input(Counter);
    });
    assert!(rec.has_producer());
    assert!(drain_errors(&mut rec).is_empty());
}

/// Runs `wire` on a fresh buffered `TestRecord` with connectors
/// `[mqtt, knx]`, so a handle with index 1 really selects `knx`.
fn registered_mqtt_knx(
    wire: impl FnOnce(&mut RecordRegistrar<'_, TestRecord>),
) -> crate::typed_record::TypedRecord<TestRecord> {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));
    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> = vec![
        Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        }),
        Box::new(MockConnectorBuilder {
            scheme: "knx".to_string(),
        }),
    ];
    let extensions = crate::extensions::Extensions::new();
    wire(&mut make_registrar(&mut rec, &builders, &extensions));
    rec
}

fn handle(builder: u32, index: u16) -> crate::ConnectorHandle {
    crate::ConnectorHandle { builder, index }
}

#[test]
fn handle_links_take_the_connector_scheme() {
    let (mqtt, knx) = (handle(TEST_BUILDER, 0), handle(TEST_BUILDER, 1));
    let mut rec = registered_mqtt_knx(|reg| {
        reg.input(InboundLink::on(knx, "1/0/7").with_deserializer(de))
            .output(OutboundLink::on(knx, "1/2/3").with_serializer(ser))
            // A full URL, as a `#[link_address]` key gives, when it matches.
            .output(OutboundLink::on(mqtt, "mqtt://sensors/temp").with_serializer(ser));
    });

    assert!(drain_errors(&mut rec).is_empty());
    assert_eq!(rec.inbound_connectors()[0].url.to_string(), "knx://1/0/7");
    let out: Vec<String> = rec
        .outbound_connectors()
        .iter()
        .map(|l| l.url.to_string())
        .collect();
    assert_eq!(out, ["knx://1/2/3", "mqtt://sensors/temp"]);
}

#[test]
fn handle_links_reject_wrong_scheme_and_foreign_handles() {
    let (mqtt, knx) = (handle(TEST_BUILDER, 0), handle(TEST_BUILDER, 1));
    let foreign = handle(TEST_BUILDER + 1, 0);
    let out_of_range = handle(TEST_BUILDER, 7);
    let mut rec = registered_mqtt_knx(|reg| {
        reg.output(OutboundLink::on(mqtt, "knx://1/2/3").with_serializer(ser))
            .input(InboundLink::on(knx, "mqtt://x").with_deserializer(de))
            .output(OutboundLink::on(foreign, "a").with_serializer(ser))
            .input(InboundLink::on(foreign, "a").with_deserializer(de))
            .output(OutboundLink::on(out_of_range, "a").with_serializer(ser));
    });

    assert!(rec.outbound_connectors().is_empty());
    assert!(rec.inbound_connectors().is_empty());
    let errors: Vec<(Option<String>, String)> = drain_errors(&mut rec)
        .into_iter()
        .map(|e| (e.url, e.message))
        .collect();
    let foreign_msg = "Connector handle belongs to a different AimDbBuilder";
    let expected = [
        (
            "knx://1/2/3",
            "Address scheme 'knx' does not match the connector's scheme 'mqtt'",
        ),
        (
            "mqtt://x",
            "Address scheme 'mqtt' does not match the connector's scheme 'knx'",
        ),
        ("a", foreign_msg),
        ("a", foreign_msg),
        ("a", foreign_msg),
    ];
    let expected: Vec<(Option<String>, String)> = expected
        .iter()
        .map(|(url, msg)| (Some(url.to_string()), msg.to_string()))
        .collect();
    assert_eq!(errors, expected);
}

#[test]
fn finish_with_unregistered_scheme_records_error() {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));

    // No connector builders registered at all
    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> = vec![];
    let extensions = crate::extensions::Extensions::new();

    let mut reg = make_registrar(&mut rec, &builders, &extensions);
    reg.link_to("mqtt://broker/topic")
        .with_serializer(|_ctx, r: &TestRecord| Ok(r.value.to_le_bytes().to_vec()))
        .finish();

    assert!(rec.outbound_connectors().is_empty());
    let errors = drain_errors(&mut rec);
    assert_eq!(errors.len(), 1);
    assert!(errors[0]
        .message
        .contains("No connector registered for scheme 'mqtt'"));
    assert_eq!(errors[0].record_key, "test::Record");
}

// ====================================================================
// Writer-exclusivity tests (.source / .transform / .link_from)
// ====================================================================

/// Helper: build a `TransformDescriptor` with a no-op spawn function.
fn dummy_transform_descriptor() -> crate::transform::TransformDescriptor<TestRecord> {
    crate::transform::TransformDescriptor::<TestRecord> {
        input_keys: vec![],
        input_types: vec![],
        build_fn: Box::new(|_p, _db, _output_key, _profiling| {
            crate::transform::CollectedTransform {
                task_future: Box::pin(async {}),
                fanin_futures: vec![],
            }
        }),
    }
}

#[test]
fn cross_stage_registrations_record_without_setter_errors() {
    // Cross-stage exclusivity (.source()/.transform()/.link_from()) is
    // validated by build(), not by the setters: conflicting registrations
    // are all recorded so build() can report the conflict with the record
    // key attached.
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));
    rec.set_producer(|_ctx, _p| async move {});

    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();

    let mut reg = make_registrar(&mut rec, &builders, &extensions);
    reg.link_from("mqtt://broker/topic")
        .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
        .finish();

    assert!(rec.has_producer());
    assert_eq!(rec.inbound_connectors().len(), 1);
    assert!(
        drain_errors(&mut rec).is_empty(),
        "no setter-level cross-stage errors; build() reports the conflict"
    );
}

#[test]
fn duplicate_source_and_transform_record_errors() {
    // Same-stage duplicates are still caught at registration time: a
    // second .source()/.transform() would silently overwrite the first.
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));

    rec.set_producer(|_ctx, _p| async move {});
    rec.set_producer(|_ctx, _p| async move {});
    rec.set_transform(dummy_transform_descriptor());
    rec.set_transform(dummy_transform_descriptor());

    let errors = drain_errors(&mut rec);
    assert_eq!(errors.len(), 2);
    assert!(errors[0].message.contains("already has a producer service"));
    assert!(errors[1]
        .message
        .contains("already has a .transform(); only one is allowed"));
}

#[test]
fn multiple_link_from_allowed() {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));

    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> =
        vec![Box::new(MockConnectorBuilder {
            scheme: "mqtt".to_string(),
        })];
    let extensions = crate::extensions::Extensions::new();
    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    // Chained via finish() → &mut RecordRegistrar …
    reg.link_from("mqtt://broker/topic-a")
        .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
        .finish()
        .link_from("mqtt://broker/topic-b")
        .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
        .finish();

    // … and as separate statements: each call takes a fresh borrow, so
    // the registrar is reusable after a chain ends.
    reg.link_from("mqtt://broker/topic-c")
        .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
        .finish();
    reg.with_name("third-link");

    assert_eq!(rec.inbound_connectors().len(), 3);
}

/// Registrar methods take fresh borrows: separate
/// statements in a configure-style closure must compile.
#[test]
fn registrar_allows_separate_statements() {
    let mut rec = crate::typed_record::TypedRecord::<TestRecord>::new();
    rec.set_buffer(Box::new(MockBuffer));

    let builders: Vec<Box<dyn crate::connector::ConnectorBuilder>> = vec![];
    let extensions = crate::extensions::Extensions::new();
    let mut reg = make_registrar(&mut rec, &builders, &extensions);

    reg.source(|_ctx, _p| async move {});
    reg.tap(|_ctx, _c| async move {});

    assert!(rec.has_producer());
    assert_eq!(rec.consumer_count(), 1);
}
