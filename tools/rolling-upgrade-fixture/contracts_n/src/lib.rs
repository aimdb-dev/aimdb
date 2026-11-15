#![no_std]
//! An independently compiled revision of the fleet's contracts.

use aimdb_data_contracts::{migration_chain, SchemaType};
use serde::{Deserialize, Serialize};

pub const INDOOR_TEMP: aimdb_data_contracts::Key<Temperature> =
    aimdb_data_contracts::Key::new("temp.indoor").with_link(INDOOR_URL);
pub const INDOOR_URL: &str = "mqtt://rolling/indoor";
pub const RETURN_TEMP: aimdb_data_contracts::Key<Temperature> =
    aimdb_data_contracts::Key::new("temp.return").with_link(RETURN_URL);
pub const RETURN_URL: &str = "mqtt://rolling/return";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemperatureV1 {
    pub temp: f32,
}
impl SchemaType for TemperatureV1 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 1;
}

pub type Temperature = TemperatureV1;
aimdb_data_contracts::catalog! { contracts: [TemperatureV1], helpers: [], }
migration_chain! { type Current = TemperatureV1; steps {} }

/// Firmware's actual link declaration, also compiled for Cortex-M.
pub fn publish<
    C: aimdb_data_contracts::LinkCodec<Temperature> + 'static,
    S: aimdb_core::SchemaState,
>(
    r: &mut aimdb_core::RecordRegistrar<'_, Temperature, S>,
    codec: C,
) {
    use aimdb_data_contracts::{EnvelopeCarrier, EnvelopeLinkExt, LinkCodecBuilderExt};
    r.link_to_key()
        .with_link_codec(codec)
        .with_envelope(EnvelopeCarrier::Inline)
        .finish();
}

pub fn receive<
    C: aimdb_data_contracts::LinkCodec<Temperature> + 'static,
    S: aimdb_core::SchemaState,
>(
    r: &mut aimdb_core::RecordRegistrar<'_, Temperature, S>,
    codec: C,
) {
    use aimdb_data_contracts::{EnvelopeCarrier, EnvelopeLinkExt, LinkCodecBuilderExt};
    r.link_from_key()
        .with_link_codec(codec)
        .with_envelope(EnvelopeCarrier::Inline)
        .finish();
}
