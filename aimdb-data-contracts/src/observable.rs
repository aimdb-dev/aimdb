//! Tests for the Observable trait.

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
}
