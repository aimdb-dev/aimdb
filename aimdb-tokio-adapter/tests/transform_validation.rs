//! Build-time validation of transform and join input types (issue #330).
//!
//! A transform reading its input as a type the input record does not hold
//! used to build fine and then silently never run. `build()` now rejects it,
//! naming the output record, the input key and both types.

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::{AimDbBuilder, DbError};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

// ---------------------------------------------------------------------------
// Record types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Celsius(f64);

#[derive(Clone, Debug, PartialEq)]
struct Fahrenheit(f64);

#[derive(Clone, Debug, PartialEq)]
struct Pascal(f64);

/// Produces `value` on `input` until `out` yields the transform's first
/// output, which also covers the transform subscribing to its input late.
async fn first_output<I, O>(
    db: &aimdb_core::AimDb,
    input: &str,
    value: I,
    out: &mut aimdb_core::buffer::Reader<O>,
) -> O
where
    I: Send + Sync + Clone + std::fmt::Debug + 'static,
    O: Send + Sync + Clone + std::fmt::Debug + 'static,
{
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            db.produce::<I>(input, value.clone()).unwrap();
            if let Ok(Ok(v)) = tokio::time::timeout(Duration::from_millis(50), out.recv()).await {
                return v.value;
            }
        }
    })
    .await
    .expect("transform output within 2 s")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transform_with_mistyped_input_is_rejected_by_build() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Celsius>("temp", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure::<Celsius>("temp_doubled", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            // Wrong input type: "temp" holds Celsius, not Pascal.
            .transform::<Pascal, _>("temp", |b| b.map(|p: &Pascal| Some(Celsius(p.0 * 2.0))));
    });

    let Err(DbError::InvalidConfiguration { errors }) = builder.build().await else {
        panic!("build() must fail with InvalidConfiguration");
    };
    assert_eq!(errors.len(), 1, "got: {errors:?}");
    assert_eq!(errors[0].record_key, "temp_doubled");
    let message = &errors[0].message;
    assert!(message.contains("'temp'"), "got: {message}");
    assert!(message.contains("Pascal"), "got: {message}");
    assert!(message.contains("Celsius"), "got: {message}");
}

#[tokio::test]
async fn join_with_one_mistyped_input_is_rejected_by_build() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Celsius>("temp", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure::<Pascal>("pressure", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure::<Celsius>("combined", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            .transform_join(|b| {
                b.input::<Celsius>("temp")
                    // Wrong input type: "pressure" holds Pascal, not Celsius.
                    .input::<Celsius>("pressure")
                    .on_triggers(
                        |mut rx, _producer| async move { while rx.recv().await.is_ok() {} },
                    )
            });
    });

    let Err(DbError::InvalidConfiguration { errors }) = builder.build().await else {
        panic!("build() must fail with InvalidConfiguration");
    };
    assert_eq!(errors.len(), 1, "got: {errors:?}");
    assert_eq!(errors[0].record_key, "combined");
    let message = &errors[0].message;
    assert!(message.contains("'pressure'"), "got: {message}");
    assert!(!message.contains("'temp'"), "got: {message}");
    assert!(message.contains("Celsius"), "got: {message}");
    assert!(message.contains("Pascal"), "got: {message}");
}

#[tokio::test]
async fn transform_with_matching_input_type_builds_and_runs() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Celsius>("temp", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure::<Celsius>("temp_doubled", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            .transform::<Celsius, _>("temp", |b| b.map(|c: &Celsius| Some(Celsius(c.0 * 2.0))));
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());
    let mut out = db.subscribe::<Celsius>("temp_doubled").unwrap();

    let got = first_output(&db, "temp", Celsius(21.0), &mut out).await;
    assert_eq!(got, Celsius(42.0));
}

/// The intended shape of a unit conversion: the transform's type parameter is
/// the *input* record's type (`Celsius`), the closure returns the *output*
/// record's type (`Fahrenheit`). Input and output types differ, which the
/// check must allow — it compares the input only against the input record.
#[tokio::test]
async fn unit_conversion_transform_builds_and_runs() {
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure::<Celsius>("temp", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure::<Fahrenheit>("temp_f", |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            .transform::<Celsius, _>("temp", |b| {
                b.map(|c: &Celsius| Some(Fahrenheit(c.0 * 9.0 / 5.0 + 32.0)))
            });
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());
    let mut out = db.subscribe::<Fahrenheit>("temp_f").unwrap();

    let got = first_output(&db, "temp", Celsius(100.0), &mut out).await;
    assert_eq!(got, Fahrenheit(212.0));
}
