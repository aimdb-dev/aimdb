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
            let record_key_1 = aimdb_core::Name::<I>::new(input);
            db.produce(record_key_1, value.clone()).unwrap();
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
    let key_temp_1 = aimdb_core::Name::<Celsius>::new("temp");
    let key_temp_doubled_2 = aimdb_core::Name::<Celsius>::new("temp_doubled");
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_temp_1, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure(key_temp_doubled_2, |reg| {
        let key_temp_3 = aimdb_core::Name::<Pascal>::new("temp");
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            // Wrong input type: "temp" holds Celsius, not Pascal.
            .transform(key_temp_3, |b| b.map(|p: &Pascal| Some(Celsius(p.0 * 2.0))));
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
    let key_temp_4 = aimdb_core::Name::<Celsius>::new("temp");
    let key_pressure_5 = aimdb_core::Name::<Pascal>::new("pressure");
    let key_combined_6 = aimdb_core::Name::<Celsius>::new("combined");
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_temp_4, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure(key_pressure_5, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure(key_combined_6, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            .transform_join(|b| {
                let key_temp_7 = aimdb_core::Name::<Celsius>::new("temp");
                let key_pressure_8 = aimdb_core::Name::<Celsius>::new("pressure");
                b.input(key_temp_7)
                    // Wrong input type: "pressure" holds Pascal, not Celsius.
                    .input(key_pressure_8)
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
    let key_temp_9 = aimdb_core::Name::<Celsius>::new("temp");
    let key_temp_doubled_10 = aimdb_core::Name::<Celsius>::new("temp_doubled");
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_temp_9, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure(key_temp_doubled_10, |reg| {
        let key_temp_11 = aimdb_core::Name::<Celsius>::new("temp");
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            .transform(key_temp_11, |b| {
                b.map(|c: &Celsius| Some(Celsius(c.0 * 2.0)))
            });
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());
    let record_key_2 = aimdb_core::Name::<Celsius>::new("temp_doubled");
    let mut out = db.subscribe(record_key_2).unwrap();

    let got = first_output(&db, "temp", Celsius(21.0), &mut out).await;
    assert_eq!(got, Celsius(42.0));
}

/// The intended shape of a unit conversion: the transform's type parameter is
/// the *input* record's type (`Celsius`), the closure returns the *output*
/// record's type (`Fahrenheit`). Input and output types differ, which the
/// check must allow — it compares the input only against the input record.
#[tokio::test]
async fn unit_conversion_transform_builds_and_runs() {
    let key_temp_12 = aimdb_core::Name::<Celsius>::new("temp");
    let key_temp_f_13 = aimdb_core::Name::<Fahrenheit>::new("temp_f");
    let runtime = Arc::new(TokioAdapter::new().unwrap());
    let mut builder = AimDbBuilder::new().runtime(runtime);

    builder.configure(key_temp_12, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 });
    });
    builder.configure(key_temp_f_13, |reg| {
        let key_temp_14 = aimdb_core::Name::<Celsius>::new("temp");
        reg.buffer(BufferCfg::SpmcRing { capacity: 8 })
            .transform(key_temp_14, |b| {
                b.map(|c: &Celsius| Some(Fahrenheit(c.0 * 9.0 / 5.0 + 32.0)))
            });
    });

    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());
    let record_key_3 = aimdb_core::Name::<Fahrenheit>::new("temp_f");
    let mut out = db.subscribe(record_key_3).unwrap();

    let got = first_output(&db, "temp", Celsius(100.0), &mut out).await;
    assert_eq!(got, Fahrenheit(212.0));
}
