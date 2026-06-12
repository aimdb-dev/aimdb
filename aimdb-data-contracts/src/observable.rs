//! Observable helper functions for data contracts.
//!
//! Provides tap functions for logging observable data types.

extern crate alloc;

use crate::Observable;

// ═══════════════════════════════════════════════════════════════════
// LOG TAP (feature = "observable")
// ═══════════════════════════════════════════════════════════════════

/// Generic logging tap for Observable types.
///
/// This function can be used with any type implementing `Observable` to
/// log observations as they flow through the mesh. It uses `format_log()`
/// to produce human-readable output.
///
/// # Example
///
/// ```no_run
/// use aimdb_data_contracts::log_tap;
/// # use aimdb_core::AimDbBuilder;
/// # use aimdb_data_contracts::{Observable, SchemaType};
/// # #[derive(Clone, Debug)]
/// # struct Temperature { celsius: f32 }
/// # impl SchemaType for Temperature { const NAME: &'static str = "temperature"; }
/// # impl Observable for Temperature {
/// #     type Signal = f32;
/// #     fn signal(&self) -> f32 { self.celsius }
/// # }
/// # fn wire(builder: &mut AimDbBuilder) {
/// builder.configure::<Temperature>("node.alpha", |reg| {
///     // .buffer(BufferCfg::SingleLatest) — via your runtime adapter's ext trait
///     reg.tap(|ctx, consumer| log_tap(ctx, consumer, "alpha"));
/// });
/// # }
/// ```
#[cfg(feature = "observable")]
pub async fn log_tap<T>(
    ctx: aimdb_core::RuntimeContext,
    consumer: aimdb_core::typed_api::Consumer<T>,
    node_id: &'static str,
) where
    T: Observable + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    let log = ctx.log();

    let mut reader = consumer.subscribe();

    while let Ok(value) = reader.recv().await {
        log.info(&value.format_log(node_id));
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

        fn signal(&self) -> f32 {
            self.value
        }
    }

    #[test]
    fn test_signal_extraction() {
        let sensor = TestSensor { value: 42.5 };
        assert_eq!(sensor.signal(), 42.5);
    }

    #[test]
    fn test_threshold_comparison() {
        let sensor = TestSensor { value: 25.0 };

        // Above threshold
        assert!(sensor.signal() > 20.0);

        // Below threshold
        assert!(sensor.signal() < 30.0);

        // In range
        let s = sensor.signal();
        assert!((20.0..=30.0).contains(&s));
    }

    #[test]
    fn test_default_icon_and_unit() {
        assert_eq!(TestSensor::ICON, "📊");
        assert_eq!(TestSensor::UNIT, "");
    }

    #[test]
    fn test_format_log_default() {
        #[derive(Debug)]
        struct DebugSensor {
            value: f32,
        }

        impl SchemaType for DebugSensor {
            const NAME: &'static str = "debug_sensor";
        }

        impl Observable for DebugSensor {
            type Signal = f32;
            fn signal(&self) -> f32 {
                self.value
            }
        }

        let sensor = DebugSensor { value: 42.5 };
        let log = sensor.format_log("node1");
        assert!(log.contains("📊"));
        assert!(log.contains("[node1]"));
        assert!(log.contains("42.5"));
    }
}
