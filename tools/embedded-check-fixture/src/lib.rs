//! Build-only fixture for the embedded checks.
//!
//! It stands in for an Embassy application's contracts and wiring, so that CI
//! compiles, on `thumbv7em-none-eabihf`, what an MCU application writes:
//!
//! - a contract with a user-written migration chain (`migratable`);
//! - two sourced records and a `transform_join` over them on the Embassy
//!   adapter;
//! - with `sim`, the same records produced by `.simulate()`.
//!
//! `make check-no-sim` uses it to prove that the default graph has no `rand`
//! and the `sim` graph has. Nothing here is run.

#![no_std]

extern crate alloc;

pub mod contracts;

use aimdb_core::AimDbBuilder;
use aimdb_embassy_adapter::{EmbassyBufferType, EmbassyRecordRegistrarExtCustom};

#[cfg(not(feature = "sim"))]
use aimdb_core::{Producer, RuntimeContext};
#[cfg(feature = "sim")]
use aimdb_data_contracts::SimulatableRegistrarExt;

pub use contracts::{DewPoint, Humidity, Temperature};

/// Record keys.
pub const TEMPERATURE: &str = "fixture.temperature";
pub const HUMIDITY: &str = "fixture.humidity";
pub const DEW_POINT: &str = "fixture.dew_point";

/// Registers two sensor records and a dew point joined from them.
pub fn configure(builder: &mut AimDbBuilder) {
    builder.configure::<Temperature>(TEMPERATURE, |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing);
        #[cfg(feature = "sim")]
        reg.simulate(sim::temperature(), sim::rng(42));
        #[cfg(not(feature = "sim"))]
        reg.source(read_temperature);
    });

    builder.configure::<Humidity>(HUMIDITY, |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing);
        #[cfg(feature = "sim")]
        reg.simulate(sim::humidity(), sim::rng(84));
        #[cfg(not(feature = "sim"))]
        reg.source(read_humidity);
    });

    builder.configure::<DewPoint>(DEW_POINT, |reg| {
        reg.buffer_sized::<8, 1>(EmbassyBufferType::SpmcRing)
            .transform_join(|b| {
                b.input::<Temperature>(TEMPERATURE)
                    .input::<Humidity>(HUMIDITY)
                    .on_triggers(|mut rx, producer| async move {
                        let mut last_temp: Option<Temperature> = None;
                        let mut last_hum: Option<Humidity> = None;
                        while let Ok(trigger) = rx.recv().await {
                            match trigger.index() {
                                0 => last_temp = trigger.as_input::<Temperature>().cloned(),
                                1 => last_hum = trigger.as_input::<Humidity>().cloned(),
                                _ => {}
                            }
                            if let (Some(t), Some(h)) = (&last_temp, &last_hum) {
                                producer.produce(DewPoint::from_readings(t, h));
                            }
                        }
                    })
            });
    });
}

/// The production source: a fixed reading where a real board reads a sensor.
#[cfg(not(feature = "sim"))]
async fn read_temperature(ctx: RuntimeContext, producer: Producer<Temperature>) {
    loop {
        let now = ctx.time().now().as_nanos() / 1_000_000;
        producer.produce(Temperature::new(22.0, now));
        ctx.time().sleep_secs(5).await;
    }
}

#[cfg(not(feature = "sim"))]
async fn read_humidity(ctx: RuntimeContext, producer: Producer<Humidity>) {
    loop {
        let now = ctx.time().now().as_nanos() / 1_000_000;
        producer.produce(Humidity {
            percent: 55.0,
            timestamp: now,
        });
        ctx.time().sleep_secs(5).await;
    }
}

#[cfg(feature = "sim")]
mod sim {
    use aimdb_data_contracts::{RandomWalkParams, SimProfile};
    use rand::SeedableRng;

    pub fn rng(seed: u8) -> rand::rngs::SmallRng {
        rand::rngs::SmallRng::from_seed([seed; 32])
    }

    pub fn temperature() -> SimProfile<RandomWalkParams> {
        SimProfile {
            interval_ms: 5000,
            params: RandomWalkParams {
                base: 22.0,
                variation: 5.0,
                step: 0.3,
                trend: 0.0,
            },
        }
    }

    pub fn humidity() -> SimProfile<RandomWalkParams> {
        SimProfile {
            interval_ms: 5000,
            params: RandomWalkParams {
                base: 55.0,
                variation: 15.0,
                step: 0.3,
                trend: 0.0,
            },
        }
    }
}
