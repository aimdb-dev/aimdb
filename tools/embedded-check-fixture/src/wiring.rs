//! Records, sources, links and a join on the Embassy adapter.

use aimdb_core::AimDbBuilder;
use aimdb_data_contracts::LinkableRegistrarExt;
use aimdb_embassy_adapter::{EmbassyBufferType, EmbassyRecordRegistrarExtCustom};

#[cfg(not(feature = "sim"))]
use aimdb_core::{Producer, RuntimeContext};
#[cfg(feature = "sim")]
use aimdb_data_contracts::SimulatableRegistrarExt;

use crate::contracts::{DewPoint, Humidity, Temperature};

/// Record keys.
pub const TEMPERATURE: aimdb_data_contracts::Key<Temperature> =
    aimdb_data_contracts::Key::new("fixture.temperature");
pub const HUMIDITY: aimdb_data_contracts::Key<Humidity> =
    aimdb_data_contracts::Key::new("fixture.humidity");
pub const DEW_POINT: aimdb_data_contracts::Key<DewPoint> =
    aimdb_data_contracts::Key::new("fixture.dew_point");

/// Where each record is published, encoded by its contract's `Linkable`.
pub const TEMPERATURE_URL: &str = "mqtt://fixture/temperature";
pub const HUMIDITY_URL: &str = "mqtt://fixture/humidity";
pub const DEW_POINT_URL: &str = "mqtt://fixture/dew_point";

/// Registers two sensor records and a dew point joined from them.
pub fn configure(builder: &mut AimDbBuilder) {
    builder.configure(TEMPERATURE, |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing);
        #[cfg(feature = "sim")]
        reg.simulate(sim::temperature(), sim::rng(42));
        #[cfg(not(feature = "sim"))]
        reg.source(read_temperature);
        reg.linked_to(TEMPERATURE_URL);
    });

    builder.configure(HUMIDITY, |reg| {
        reg.buffer_sized::<16, 2>(EmbassyBufferType::SpmcRing);
        #[cfg(feature = "sim")]
        reg.simulate(sim::humidity(), sim::rng(84));
        #[cfg(not(feature = "sim"))]
        reg.source(read_humidity);
        reg.linked_to(HUMIDITY_URL);
    });

    builder.configure(DEW_POINT, |reg| {
        reg.buffer_sized::<8, 1>(EmbassyBufferType::SpmcRing)
            .transform_join(|b| {
                b.input(TEMPERATURE)
                    .input(HUMIDITY)
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
            })
            .linked_to(DEW_POINT_URL);
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

/// Build-only proof: the same envelope admission compiles on Cortex-M.
/// In an application this policy is created once while registering the link.
pub fn temperature_admission(
) -> aimdb_data_contracts::LinkContract<Temperature, aimdb_data_contracts::link_codecs::Json> {
    aimdb_data_contracts::LinkContract::inline_from::<3>(
        aimdb_core::envelope::PayloadCodec::Json,
        aimdb_data_contracts::link_codecs::Json,
    )
}

pub fn admit_temperature_frame(
    frame: &[u8],
    link: &aimdb_data_contracts::LinkContract<Temperature, aimdb_data_contracts::link_codecs::Json>,
) -> aimdb_data_contracts::Admit<Temperature> {
    aimdb_data_contracts::admit_frame(frame, link)
}

/// Registration-time envelope strategies compile on the MCU too. Called by a
/// build-only fixture; a real application registers its MQTT connector first.
pub fn configure_versioned_links(builder: &mut AimDbBuilder) {
    let key_fixture_versioned_input_1 =
        aimdb_core::Name::<Temperature>::new("fixture.versioned.input");
    let key_fixture_versioned_output_2 =
        aimdb_core::Name::<Temperature>::new("fixture.versioned.output");
    use aimdb_data_contracts::{
        link_codecs::Json, EnvelopeCarrier, EnvelopeLinkExt, LinkCodecBuilderExt,
        VersionedInboundExt, VersionedOutboundExt,
    };
    builder.configure(key_fixture_versioned_input_1, |r| {
        r.buffer_sized::<4, 1>(EmbassyBufferType::SpmcRing)
            .link_from("mqtt://fixture/upgrade")
            .with_link_codec(Json)
            .with_envelope(EnvelopeCarrier::Inline)
            .accepts_from::<3>()
            .finish();
    });
    builder.configure(key_fixture_versioned_output_2, |r| {
        r.buffer_sized::<4, 1>(EmbassyBufferType::SpmcRing)
            .link_to("mqtt://fixture/legacy")
            .with_link_codec(Json)
            .with_envelope(EnvelopeCarrier::Inline)
            .sends_as::<3>()
            .finish();
    });
}

/// Monomorphize the rolling fixture's exact firmware/hub declarations on MCU.
pub fn configure_rolling_fixture(builder: &mut AimDbBuilder) {
    let key_rolling_firmware_out_3 =
        aimdb_core::Name::<firmware::Temperature>::new("rolling.firmware.out");
    let key_rolling_firmware_in_4 =
        aimdb_core::Name::<firmware::Temperature>::new("rolling.firmware.in");
    let key_rolling_hub_in_5 = aimdb_core::Name::<hub::Temperature>::new("rolling.hub.in");
    let key_rolling_hub_out_6 = aimdb_core::Name::<hub::Temperature>::new("rolling.hub.out");
    use aimdb_data_contracts::link_codecs::{Cdr, Postcard};
    use rolling_contracts_n as firmware;
    use rolling_contracts_n1 as hub;
    builder.configure(key_rolling_firmware_out_3, |r| {
        r.buffer_sized::<4, 1>(EmbassyBufferType::SpmcRing);
        firmware::publish(r, Postcard::<64>);
    });
    builder.configure(key_rolling_firmware_in_4, |r| {
        r.buffer_sized::<4, 1>(EmbassyBufferType::SpmcRing);
        firmware::receive(r, Postcard::<64>);
    });
    builder.configure(key_rolling_hub_in_5, |r| {
        r.buffer_sized::<4, 1>(EmbassyBufferType::SpmcRing);
        hub::receive(r, Cdr::<64>);
    });
    builder.configure(key_rolling_hub_out_6, |r| {
        r.buffer_sized::<4, 1>(EmbassyBufferType::SpmcRing);
        hub::send_to_firmware(r, Cdr::<64>);
    });
}
