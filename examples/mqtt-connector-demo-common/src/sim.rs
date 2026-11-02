//! Simulated thermometers: one input type for every runtime.
//!
//! [`SimThermometers`] stands in for a board: it hands out one owned
//! [`SimChannel`] per sensor. [`Thermometer::on`] takes a channel by value and
//! is an [`Input`] for every record type that can be built from a
//! [`Reading`], so the same input serves Tokio and Embassy alike:
//!
//! ```rust,ignore
//! let SimThermometers { indoor, outdoor, server_room } = SimThermometers::new();
//!
//! builder.configure::<Temperature>(SensorKey::TempIndoor, |reg| {
//!     reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
//!         .input(Thermometer::on(indoor));
//! });
//! ```
//!
//! A channel is not `Clone`, so wiring one sensor into two records is a
//! compile error (`use of moved value`) rather than two producers reading
//! one device.

use aimdb_core::{Input, RecordRegistrar};
use core::fmt::Debug;

use crate::types::Temperature;

/// What a thermometer yields: one temperature reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// Which sensor took the reading (e.g. "indoor-001").
    pub sensor_id: &'static str,
    /// Temperature in degrees Celsius.
    pub celsius: f32,
}

impl From<Reading> for Temperature {
    fn from(reading: Reading) -> Self {
        Temperature::new(reading.sensor_id, reading.celsius)
    }
}

/// One simulated sensor. Deliberately neither `Clone` nor `Copy`: an input
/// takes its channel by value.
#[derive(Debug)]
pub struct SimChannel {
    sensor_id: &'static str,
    /// First reading.
    base: f32,
    /// Added per reading; the sensor cycles through five readings.
    step: f32,
}

/// The simulated thermometers, one field per sensor.
#[derive(Debug)]
pub struct SimThermometers {
    /// Indoor: 22.0–24.0 °C.
    pub indoor: SimChannel,
    /// Outdoor: 5.0–9.0 °C.
    pub outdoor: SimChannel,
    /// Server room: 18.0–18.8 °C.
    pub server_room: SimChannel,
}

impl SimThermometers {
    /// The demo's three sensors.
    pub fn new() -> Self {
        let channel = |sensor_id, base, step| SimChannel {
            sensor_id,
            base,
            step,
        };
        Self {
            indoor: channel("indoor-001", 22.0, 0.5),
            outdoor: channel("outdoor-001", 5.0, 1.0),
            server_room: channel("server-room-001", 18.0, 0.2),
        }
    }
}

impl Default for SimThermometers {
    fn default() -> Self {
        Self::new()
    }
}

/// A thermometer input on one [`SimChannel`]: produces a reading every two
/// seconds, forever unless limited with [`readings`](Self::readings).
#[derive(Debug)]
pub struct Thermometer {
    channel: SimChannel,
    period_secs: u64,
    readings: Option<usize>,
}

impl Thermometer {
    /// A thermometer reading `channel`.
    pub fn on(channel: SimChannel) -> Self {
        Self {
            channel,
            period_secs: 2,
            readings: None,
        }
    }

    /// Seconds between readings (default 2).
    pub fn every_secs(mut self, secs: u64) -> Self {
        self.period_secs = secs;
        self
    }

    /// Stops after `count` readings instead of running forever.
    pub fn readings(mut self, count: usize) -> Self {
        self.readings = Some(count);
        self
    }
}

impl<T> Input<T> for Thermometer
where
    T: From<Reading> + Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T>) {
        let Thermometer {
            channel,
            period_secs,
            readings,
        } = self;
        reg.source(move |ctx, producer| async move {
            let log = ctx.log();
            log.info(&alloc::format!("🌡️  {} started", channel.sensor_id));

            for i in (0..5u8).cycle().take(readings.unwrap_or(usize::MAX)) {
                let reading = Reading {
                    sensor_id: channel.sensor_id,
                    celsius: channel.base + f32::from(i) * channel.step,
                };
                log.info(&alloc::format!(
                    "🌡️  {} producing: {:.1}°C",
                    reading.sensor_id,
                    reading.celsius
                ));
                producer.produce(T::from(reading));
                ctx.time().sleep_secs(period_secs).await;
            }

            log.info(&alloc::format!("✅ {} finished", channel.sensor_id));
        });
    }
}
