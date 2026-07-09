//! Observable registrar extension + logging tap for data contracts.
//!
//! Implementing [`Observable`](crate::Observable) unlocks one verb —
//! [`ObservableRegistrarExt::observe`] — which feeds the record's domain signal
//! into core signal-gauge metrics. `.log(node_id)` is the human-readable
//! companion for console watching.

extern crate alloc;

use aimdb_core::typed_api::RecordRegistrar;

use crate::Observable;

// ═══════════════════════════════════════════════════════════════════
// LOG TAP
// ═══════════════════════════════════════════════════════════════════

/// Generic logging tap for [`Observable`] types.
///
/// Formats each value from `Debug` plus the schema's
/// [`SIGNAL`](Observable::SIGNAL)/[`UNIT`](Observable::UNIT) labels. Prefer
/// [`ObservableRegistrarExt::log`]; this free function remains for hand-wired
/// `.tap()` calls.
pub async fn log_tap<T>(
    ctx: aimdb_core::RuntimeContext,
    consumer: aimdb_core::typed_api::Consumer<T>,
    node_id: &str,
) where
    T: Observable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    let log = ctx.log();
    let mut reader = consumer.subscribe();

    while let Ok(value) = reader.recv().await {
        log.info(&alloc::format!(
            "[{}] {}: {:?}{}",
            node_id,
            T::SIGNAL,
            value,
            T::UNIT
        ));
    }
}

// ═══════════════════════════════════════════════════════════════════
// REGISTRAR EXTENSION: `.observe()` / `.log(node_id)`
// ═══════════════════════════════════════════════════════════════════

/// Adds `.observe()` and `.log(node_id)` to [`RecordRegistrar`] for
/// [`Observable`] types.
pub trait ObservableRegistrarExt<'a, T>
where
    T: Observable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    /// Feed `T::signal()` into the record's signal gauge (last/min/max/mean),
    /// visible via `record.list` / `record.get` / stage profiling.
    ///
    /// Recording requires `aimdb-core`'s `observability` feature: without it
    /// [`RecordRegistrar::signal_gauge`] hands back an inert gauge and this
    /// tap compiles and runs but records nothing. `.log()` does not depend on
    /// that feature.
    ///
    /// Bounded `T::Signal: Into<f64>` here (not on the trait), so `f32`/`i32`/
    /// `u32` signals qualify while a type with an exotic signal can still
    /// implement `Observable` and write its own tap.
    fn observe(&mut self) -> &mut RecordRegistrar<'a, T>
    where
        T::Signal: Into<f64>;

    /// Log each value to the runtime log, formatted from `Debug` +
    /// `SIGNAL`/`UNIT`. For humans watching a console; `.observe()` is the
    /// metrics path.
    fn log(&mut self, node_id: &'static str) -> &mut RecordRegistrar<'a, T>;
}

impl<'a, T> ObservableRegistrarExt<'a, T> for RecordRegistrar<'a, T>
where
    T: Observable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    fn observe(&mut self) -> &mut RecordRegistrar<'a, T>
    where
        T::Signal: Into<f64>,
    {
        let gauge = self.signal_gauge(T::SIGNAL, T::UNIT);
        self.tap(move |_ctx, consumer| async move {
            let mut reader = consumer.subscribe();
            while let Ok(value) = reader.recv().await {
                gauge.update(value.signal().into());
            }
        })
        .with_name("observe")
    }

    fn log(&mut self, node_id: &'static str) -> &mut RecordRegistrar<'a, T> {
        self.tap(move |ctx, consumer| log_tap(ctx, consumer, node_id))
            .with_name("log")
    }
}

// ═══════════════════════════════════════════════════════════════════
// TESTS
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use crate::{Observable, SchemaType};

    struct TestSensor {
        value: f32,
    }

    impl SchemaType for TestSensor {
        const NAME: &'static str = "test_sensor";
    }

    impl Observable for TestSensor {
        type Signal = f32;
        const UNIT: &'static str = "°C";

        fn signal(&self) -> f32 {
            self.value
        }
    }

    #[test]
    fn signal_extraction() {
        let sensor = TestSensor { value: 42.5 };
        assert_eq!(sensor.signal(), 42.5);
    }

    #[test]
    fn signal_label_defaults_to_schema_name() {
        assert_eq!(TestSensor::SIGNAL, "test_sensor");
        assert_eq!(TestSensor::UNIT, "°C");
    }

    #[test]
    fn signal_is_into_f64() {
        let sensor = TestSensor { value: 25.0 };
        let as_f64: f64 = sensor.signal().into();
        assert_eq!(as_f64, 25.0);
    }
}
