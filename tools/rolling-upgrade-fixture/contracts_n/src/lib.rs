#![no_std]
//! An independently compiled revision of the fleet's contracts.

use aimdb_data_contracts::{migration_chain, SchemaType};
use serde::{Deserialize, Serialize};

pub const INDOOR_TEMP: &str = "temp.indoor";
pub const INDOOR_URL: &str = "mqtt://rolling/indoor";
pub const RETURN_TEMP: &str = "temp.return";
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
pub fn publish<C: aimdb_data_contracts::LinkCodec<Temperature> + 'static>(
    r: &mut aimdb_core::RecordRegistrar<'_, Temperature>,
    codec: C,
) {
    use aimdb_data_contracts::{EnvelopeCarrier, EnvelopeLinkExt, LinkCodecBuilderExt};
    r.link_to(INDOOR_URL)
        .with_link_codec(codec)
        .with_envelope(EnvelopeCarrier::Inline)
        .finish();
}

pub fn receive<C: aimdb_data_contracts::LinkCodec<Temperature> + 'static>(
    r: &mut aimdb_core::RecordRegistrar<'_, Temperature>,
    codec: C,
) {
    use aimdb_data_contracts::{EnvelopeCarrier, EnvelopeLinkExt, LinkCodecBuilderExt};
    r.link_from(RETURN_URL)
        .with_link_codec(codec)
        .with_envelope(EnvelopeCarrier::Inline)
        .finish();
}
