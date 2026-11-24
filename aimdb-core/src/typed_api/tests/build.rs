//! Whole-builder checks through `AimDbBuilder::build()`: cross-stage and
//! cross-connector validation, graph and schema reporting.

use super::*;

/// The target shape from #390, through `build()`: links on a handle
/// build; the same links on another builder's handle fail it.
#[tokio::test]
async fn handle_links_build_and_foreign_handles_fail_it() {
    let mut other = crate::AimDbBuilder::new();
    let foreign = other.connect(NoopConnectorBuilder);

    for (use_foreign, expect_ok) in [(false, true), (true, false)] {
        let key_temp_indoor_1 = crate::Name::<TestRecord>::new("temp.indoor");
        let mut builder = crate::AimDbBuilder::new().runtime(Arc::new(MockRuntime));
        let mqtt = builder.connect(NoopConnectorBuilder);
        let on = if use_foreign { foreign } else { mqtt };
        builder.configure(key_temp_indoor_1, move |reg| {
            reg.buffer_raw(Box::new(MockBuffer))
                .input(InboundLink::on(on, "commands/temp/indoor").with_deserializer(de))
                .output(OutboundLink::on(on, "sensors/temp/indoor").with_serializer(ser));
        });
        let result = builder.build().await;
        if expect_ok {
            let (db, _runner) = result.expect("handle links build");
            let inbound = crate::InboundDispatch::new(&db, "mqtt", &Plus).expect("routes compile");
            assert_eq!(inbound.subscriptions()[0].as_ref(), "commands/temp/indoor");
        } else {
            let errors = config_errors(result);
            assert_eq!(errors.len(), 2, "got: {errors:?}");
            assert!(errors.iter().all(|e| e.record_key == "temp.indoor"
                && e.message == "Connector handle belongs to a different AimDbBuilder"));
        }
    }
}

/// A connector registered with `connect` serves links exactly like one
/// registered with `with_connector`.
#[tokio::test]
async fn connected_connector_serves_links() {
    let key_rec_in_2 = crate::Name::<TestRecord>::new("rec.in");
    let mut builder = crate::AimDbBuilder::new().runtime(Arc::new(MockRuntime));
    let _mqtt = builder.connect(NoopConnectorBuilder);
    builder.configure(key_rec_in_2, |reg| {
        reg.buffer_raw(Box::new(MockBuffer))
            .input(InboundLink::new("mqtt://a").with_deserializer(de));
    });
    let (db, _runner) = builder.build().await.expect("builds");
    let inbound = crate::InboundDispatch::new(&db, "mqtt", &Plus).expect("routes compile");
    assert_eq!(inbound.route_count(), 1);
}

/// Two inbound links on one record are fan-in, through `.input()` as
/// through `.link_from()`.
#[tokio::test]
async fn two_inbound_inputs_fan_in() {
    let key_rec_in_3 = crate::Name::<TestRecord>::new("rec.in");
    let mut builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(NoopConnectorBuilder);
    builder.configure(key_rec_in_3, |reg| {
        reg.buffer_raw(Box::new(MockBuffer))
            .input(InboundLink::new("mqtt://a").with_deserializer(de))
            .input(InboundLink::new("mqtt://b").with_deserializer(de));
    });
    let (db, _runner) = builder.build().await.expect("fan-in builds");
    let inbound = crate::InboundDispatch::new(&db, "mqtt", &Plus).expect("routes compile");
    assert_eq!(inbound.route_count(), 2);
}

#[tokio::test]
async fn input_source_and_link_conflict() {
    let key_rec_in_4 = crate::Name::<TestRecord>::new("rec.in");
    let mut builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(NoopConnectorBuilder);
    builder.configure(key_rec_in_4, |reg| {
        reg.buffer_raw(Box::new(MockBuffer))
            .input(Counter)
            .input(InboundLink::new("mqtt://a").with_deserializer(de));
    });
    let errors = config_errors(builder.build().await);
    assert_eq!(errors.len(), 1, "got: {errors:?}");
    assert_eq!(errors[0].record_key, "rec.in");
    assert!(
        errors[0]
            .message
            .contains("conflicting writers: record has source + inbound link"),
        "got: {}",
        errors[0].message
    );
}

// ====================================================================
// build()-level validation tests
// ====================================================================

#[derive(Debug, Clone)]
struct OtherRecord;

/// A connector that claims every route for its scheme, as KNX and the
/// session *clients* do.
struct OwningConnectorBuilder(&'static str);

impl crate::connector::ConnectorBuilder for OwningConnectorBuilder {
    fn build<'a>(
        &'a self,
        _db: &'a crate::AimDb,
    ) -> Pin<
        Box<
            dyn Future<Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn scheme(&self) -> &str {
        self.0
    }
    fn owns_scheme(&self) -> bool {
        true
    }
}

/// Two scheme-owning connectors under one scheme is a configuration error,
/// not a silently duplicated route set.
#[tokio::test]
async fn two_owning_connectors_on_one_scheme_fail_the_build() {
    let builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(OwningConnectorBuilder("knx"))
        .with_connector(OwningConnectorBuilder("knx"));

    let Err(err) = builder.build().await else {
        panic!("a second connector for an owned scheme must be rejected");
    };
    let msg = alloc::format!("{err}");
    assert!(
        msg.contains("More than one connector registered for scheme 'knx'"),
        "unexpected error: {msg}"
    );
}

/// Distinct schemes are fine — that is how a caller runs two of the same
/// transport side by side.
#[tokio::test]
async fn owning_connectors_on_distinct_schemes_build() {
    let builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(OwningConnectorBuilder("knx"))
        .with_connector(OwningConnectorBuilder("mqtt"));

    assert!(builder.build().await.is_ok());
}

/// A connector that collects no routes — a session *server*, say — may be
/// registered twice under one scheme: two endpoints onto one dispatch.
/// `owns_scheme` defaults to `false`, so this must keep working.
#[tokio::test]
async fn two_non_owning_connectors_on_one_scheme_build() {
    let builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(NoopConnectorBuilder)
        .with_connector(NoopConnectorBuilder);

    assert!(builder.build().await.is_ok());
}

/// Acceptance criterion: a builder with three distinct
/// mistakes reports all three from one `build()` call.
#[tokio::test]
async fn build_reports_all_configuration_mistakes_at_once() {
    let key_rec_a_5 = crate::Name::<TestRecord>::new("rec.a");
    let key_rec_b_6 = crate::Name::<TestRecord>::new("rec.b");
    let key_rec_c_7 = crate::Name::<TestRecord>::new("rec.c");
    let key_rec_c_8 = crate::Name::<OtherRecord>::new("rec.c");
    let mut builder = crate::AimDbBuilder::new().runtime(Arc::new(MockRuntime));

    // Mistake 1: outbound link with no serializer
    builder.configure(key_rec_a_5, |reg| {
        reg.link_to("mqtt://broker/a").finish();
    });
    // Mistake 2: two .source() registrations on one record
    builder.configure(key_rec_b_6, |reg| {
        reg.source(|_ctx, _p| async move {});
        reg.source(|_ctx, _p| async move {});
    });
    // Mistake 3: key re-registered with a different type
    builder.configure(key_rec_c_7, |_reg| {});
    builder.configure(key_rec_c_8, |_reg| {});

    let Err(err) = builder.build().await else {
        panic!("build must fail");
    };
    let crate::DbError::InvalidConfiguration { errors } = err else {
        panic!("expected InvalidConfiguration, got {err:?}");
    };
    assert_eq!(errors.len(), 3, "expected 3 errors, got: {errors:?}");
    assert!(errors.iter().any(|e| e.record_key == "rec.a"
        && e.url.as_deref() == Some("mqtt://broker/a")
        && e.message.contains("requires a serializer")));
    assert!(errors
        .iter()
        .any(|e| e.record_key == "rec.b" && e.message.contains("already has a producer service")));
    assert!(errors
        .iter()
        .any(|e| e.record_key == "rec.c" && e.message.contains("different type")));
}

/// `.buffer()` after `.link_from()` is legitimate now that the buffer
/// requirement is validated by `build()` instead of `finish()`.
#[tokio::test]
async fn buffer_after_link_from_is_valid() {
    let key_rec_x_9 = crate::Name::<TestRecord>::new("rec.x");
    let mut builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(NoopConnectorBuilder);

    builder.configure(key_rec_x_9, |reg| {
        reg.link_from("mqtt://broker/x")
            .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();
        // Buffer configured AFTER the link — order-independent.
        reg.buffer_raw(Box::new(MockBuffer));
    });

    builder.build().await.expect("build must succeed");
}

/// A linked record without a buffer fails at build() — previously this
/// panicked at spawn time, deep inside a connector factory closure.
#[tokio::test]
async fn linked_record_without_buffer_fails_build() {
    let key_rec_x_10 = crate::Name::<TestRecord>::new("rec.x");
    let mut builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(NoopConnectorBuilder);

    builder.configure(key_rec_x_10, |reg| {
        reg.link_from("mqtt://broker/x")
            .with_deserializer(|_ctx, _b: &[u8]| Ok(TestRecord { value: 0 }))
            .finish();
    });

    let Err(err) = builder.build().await else {
        panic!("build must fail");
    };
    let crate::DbError::InvalidConfiguration { errors } = err else {
        panic!("expected InvalidConfiguration, got {err:?}");
    };
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("requires a buffer"));
    assert_eq!(errors[0].record_key, "rec.x");
    assert_eq!(errors[0].url.as_deref(), Some("mqtt://broker/x"));
}

/// #332: a record's schema and its outbound link addresses reach the
/// graph node and `record.list`.
#[tokio::test]
async fn graph_node_carries_schema_and_outbound_links() {
    let key_rec_out_14 = crate::Name::<TestRecord>::new("rec.out");
    let key_rec_plain_15 = crate::Name::<TestRecord>::new("rec.plain");
    let mut builder = crate::AimDbBuilder::new()
        .runtime(Arc::new(MockRuntime))
        .with_connector(NoopConnectorBuilder);
    builder.configure(key_rec_out_14, |reg| {
        reg.buffer_raw(Box::new(MockBuffer));
        reg.schema("flow", 2).schema("flow", 2);
        reg.link_to("mqtt://plant/boiler/flow_avg")
            .with_serializer(le)
            .finish();
        reg.link_to("mqtt://plant/boiler/flow_raw")
            .with_serializer(le)
            .finish();
    });
    builder.configure(key_rec_plain_15, |_reg| {});
    let (db, _runner) = builder.build().await.expect("build must succeed");

    let graph = db.inner().dependency_graph();
    let out = graph.node("rec.out").unwrap();
    assert_eq!(
        out.schema,
        Some(crate::graph::SchemaInfo {
            name: "flow".to_string(),
            version: 2
        })
    );
    assert_eq!(
        out.outbound_links,
        [
            "mqtt://plant/boiler/flow_avg",
            "mqtt://plant/boiler/flow_raw"
        ]
    );
    assert!(out.has_outbound_link);
    let plain = graph.node("rec.plain").unwrap();
    assert_eq!(
        (plain.schema.as_ref(), plain.has_outbound_link),
        (None, false)
    );
    assert!(plain.outbound_links.is_empty());

    #[cfg(feature = "remote")]
    {
        let records = db.list_records();
        let out = records.iter().find(|r| r.record_key == "rec.out").unwrap();
        assert_eq!(out.schema_type.as_deref(), Some("flow"));
        assert_eq!(out.schema_version, Some(2));
        let plain = records
            .iter()
            .find(|r| r.record_key == "rec.plain")
            .unwrap();
        assert_eq!(
            (plain.schema_type.as_deref(), plain.schema_version),
            (None, None)
        );
    }
}

/// #332: two different schemas on one record fail `build()`.
#[tokio::test]
async fn conflicting_schemas_fail_build() {
    let key_rec_x_16 = crate::Name::<TestRecord>::new("rec.x");
    let mut builder = crate::AimDbBuilder::new().runtime(Arc::new(MockRuntime));
    builder.configure(key_rec_x_16, |reg| {
        reg.schema("flow", 1).schema("flow", 2);
    });
    let Err(crate::DbError::InvalidConfiguration { errors }) = builder.build().await else {
        panic!("build must fail");
    };
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].record_key, "rec.x");
    assert!(errors[0].message.contains("flow v1"), "{errors:?}");
}
